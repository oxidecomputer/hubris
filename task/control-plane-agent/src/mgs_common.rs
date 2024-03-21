// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, Log, MgsMessage};
use drv_caboose::{CabooseError, CabooseReader};
use drv_sprot_api::{
    CabooseOrSprotError, Fwid as SpFwid, ImageError as SpImageError,
    ImageVersion as SpImageVersion, RotBootInfo as SpRotBootInfo,
    RotBootInfoV2 as SpRotBootInfoV2, RotComponent as SpRotComponent,
    RotImageDetails as SpRotImageDetails, SlotId as SpSlotId, SpRot,
    SprotError, SprotProtocolError, SwitchDuration,
    VersionedRotBootInfo as SpVersionedRotBootInfo,
};
use drv_stm32h7_update_api::Update;
use gateway_messages::{
    CfpaPage,
    DiscoverResponse,
    Fwid as GatewayFwid,
    ImageError as GatewayImageError,
    ImageVersion as GatewayImageVersion,
    PowerState,
    RotBootInfo as GatewayRotBootInfo,
    RotBootState as GatewayRotBootState,
    RotError,
    RotImageDetails as GatewayRotImageDetails,
    RotRequest,
    RotResponse,
    RotSlotId as GatewayRotSlotId,
    RotState as GatewayRotState,
    RotStateV2 as GatewayRotStateV2,
    RotStateV3 as GatewayRotStateV3,
    RotUpdateDetails as GatewayRotUpdateDetails,
    SensorReading,
    SensorRequest,
    SensorRequestKind,
    SensorResponse,
    SpComponent as GatewaySpComponent,
    SpError as GatewaySpError,
    SpPort as GatewaySpPort,
    // SpStateV3 as GatewaySpStateV3,
    SpStateV2 as GatewaySpStateV2,
    VpdError as GatewayVpdError,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use static_assertions::const_assert;
use task_control_plane_agent_api::VpdIdentity;
use task_net_api::MacAddress;
use task_packrat_api::Packrat;
use task_sensor_api::{Sensor, SensorId};
use userlib::{kipc, sys_get_timer, task_slot};

task_slot!(PACKRAT, packrat);
task_slot!(SENSOR, sensor);
task_slot!(pub SPROT, sprot);
task_slot!(pub UPDATE_SERVER, update_server);

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    reset_component_requested: Option<GatewaySpComponent>,
    inventory: Inventory,
    base_mac_address: MacAddress,
    packrat: Packrat,
    sprot: SpRot,
    sp_update: Update,
    sensor: Sensor,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            reset_component_requested: None,
            inventory: Inventory::new(),
            base_mac_address,
            packrat: Packrat::from(PACKRAT.get_task_id()),
            sprot: SpRot::from(SPROT.get_task_id()),
            sp_update: Update::from(UPDATE_SERVER.get_task_id()),
            sensor: Sensor::from(SENSOR.get_task_id()),
        }
    }

    #[allow(dead_code)] // This function is only used by Gimlet right now
    pub(crate) fn packrat(&self) -> &Packrat {
        &self.packrat
    }

    pub(crate) fn discover(
        &mut self,
        port: GatewaySpPort,
    ) -> Result<DiscoverResponse, GatewaySpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        // We don't need to wait for packrat to be loaded: the sequencer task
        // for our board already does, and `net` waits for the sequencer before
        // starting. If we've gotten here, we've received a packet on the
        // network, which means `net` has started and the sequencer has already
        // populated packrat with what it read from our VPD.
        self.packrat.get_identity().unwrap_or_default()
    }

    pub(crate) fn sp_state(
        &mut self,
        power_state: PowerState,
    ) -> Result<GatewaySpStateV2, GatewaySpError> {
        // SpState has extra-wide fields for the serial and model number. Below
        // when we fill them in we use `usize::min` to pick the right length
        // regardless of which is longer, but really we want to know we aren't
        // truncating our values. We'll statically assert that `SpState`'s field
        // length is wider than `VpdIdentity`'s to catch this early.
        const SP_STATE_FIELD_WIDTH: usize = 32;
        const_assert!(SP_STATE_FIELD_WIDTH >= VpdIdentity::SERIAL_LEN);
        const_assert!(SP_STATE_FIELD_WIDTH >= VpdIdentity::PART_NUMBER_LEN);

        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

        let id = self.identity();

        let mut state = GatewaySpStateV2 {
            serial_number: [0; SP_STATE_FIELD_WIDTH],
            model: [0; SP_STATE_FIELD_WIDTH],
            revision: id.revision,
            hubris_archive_id: kipc::read_image_id().to_le_bytes(),
            base_mac_address: self.base_mac_address.0,
            power_state,
            rot: self
                .sprot
                .rot_boot_info()
                .map(|s| MgsRotStateV2::from(s).0)
                .map_err(RotError::from),
        };

        let n = usize::min(state.serial_number.len(), id.serial.len());
        state.serial_number[..n].copy_from_slice(&id.serial);

        let n = usize::min(state.model.len(), id.part_number.len());
        state.model[..n].copy_from_slice(&id.part_number);

        Ok(state)
    }

    #[inline(always)]
    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub(crate) fn get_component_caboose_value(
        &self,
        component: GatewaySpComponent,
        slot: u16,
        key: [u8; 4],
        buf: &mut [u8],
    ) -> Result<usize, GatewaySpError> {
        let caboose_to_sp_error = |e| {
            match e {
                CabooseError::NoSuchTag => {
                    GatewaySpError::NoSuchCabooseKey(key)
                }
                CabooseError::MissingCaboose => GatewaySpError::NoCaboose,
                CabooseError::BadChecksum => GatewaySpError::BadCabooseChecksum,
                CabooseError::TlvcReaderBeginFailed
                | CabooseError::RawReadFailed
                | CabooseError::InvalidRead
                | CabooseError::TlvcReadExactFailed => {
                    GatewaySpError::CabooseReadError
                }

                // NoImageHeader is only returned when reading the caboose of the
                // bank2 slot; it shouldn't ever be returned by the local reader.
                CabooseError::NoImageHeader => GatewaySpError::NoCaboose,
            }
        };
        let caboose_or_sprot_to_sp_error = |e| match e {
            CabooseOrSprotError::Caboose(e) => caboose_to_sp_error(e),
            CabooseOrSprotError::Sprot(e) => e.into(),
        };

        match component {
            GatewaySpComponent::SP_ITSELF => match slot {
                0 => {
                    let reader = drv_caboose_pos::CABOOSE_POS
                        .as_slice()
                        .map(CabooseReader::new)
                        .ok_or(GatewaySpError::NoCaboose)?;
                    let v = reader.get(key).map_err(caboose_to_sp_error)?;
                    let len = v.len();
                    if len > buf.len() {
                        Err(GatewaySpError::CabooseValueOverflow(len as u32))
                    } else {
                        buf[..len].copy_from_slice(v);
                        Ok(len)
                    }
                }
                1 => {
                    let len = self
                        .sp_update
                        .read_caboose_value(key, buf)
                        .map_err(caboose_to_sp_error)?;
                    Ok(len as usize)
                }
                _ => Err(GatewaySpError::InvalidSlotForComponent),
            },
            GatewaySpComponent::ROT => {
                let slot_id = slot
                    .try_into()
                    .map_err(|()| GatewaySpError::InvalidSlotForComponent)?;
                let len = self
                    .sprot
                    .read_caboose_value(
                        SpRotComponent::Hubris,
                        slot_id,
                        key,
                        buf,
                    )
                    .map_err(caboose_or_sprot_to_sp_error)?;
                Ok(len as usize)
            }
            GatewaySpComponent::STAGE0 => {
                let slot_id = slot
                    .try_into()
                    .map_err(|()| GatewaySpError::InvalidSlotForComponent)?;
                let len = self
                    .sprot
                    .read_caboose_value(
                        SpRotComponent::Stage0,
                        slot_id,
                        key,
                        buf,
                    )
                    .map_err(caboose_or_sprot_to_sp_error)?;
                Ok(len as usize)
            }
            _ => Err(GatewaySpError::RequestUnsupportedForComponent),
        }
    }

    /// If the targeted component is the SP_ITSELF, then having reset itself,
    /// it will not be able to respond to the later reset_trigger message.
    ///
    /// So, after getting an ACK for the prepare message, MGS will send and
    /// retry the reset_trigger message until it gets rejected for lack of
    /// a corresponding prepare message.
    ///
    /// If the targeted component is not the SP_ITSELF, it may still have impact
    /// on the SP if reset, either now or in a future implementation.
    /// However, for some components, the SP will be able to send an
    /// acknowledgement and retrying the trigger message will not be effective.
    /// The implementation in the control plane should handle both cases.
    pub(crate) fn reset_component_prepare(
        &mut self,
        component: GatewaySpComponent,
    ) -> Result<(), GatewaySpError> {
        self.reset_component_requested = Some(component);
        Ok(())
    }

    /// ResetComponent is used in the context of the management plane
    /// driving a firmware update.
    ///
    /// When an update is complete, or perhaps for handling update errors,
    /// the management plane will need to reset a component or change
    /// boot image selection policy and reset that component.
    ///
    /// The target of the operation is the management plane's SpComponent
    /// and firmware slot.
    /// For the RoT, that is SpComponent::ROT and slot 0(ImageA) or 1(ImageB)
    /// or SpComponent::STATE0 and slot 0.
    pub(crate) fn reset_component_trigger(
        &mut self,
        component: GatewaySpComponent,
    ) -> Result<(), GatewaySpError> {
        if self.reset_component_requested != Some(component) {
            return Err(GatewaySpError::ResetComponentTriggerWithoutPrepare);
        }
        // If we are not resetting the SP_ITSELF, then we may come back here
        // to reset something else or to run another prepare/trigger on
        // the same component.
        self.reset_component_requested = None;

        // Resetting the SP through reset_component() is
        // the same as through reset() until transient bank selection is
        // figured out for the SP.
        match component {
            GatewaySpComponent::SP_ITSELF => {
                task_jefe_api::Jefe::from(crate::JEFE.get_task_id())
                    .request_reset();
                // If `request_reset()` returns,
                // something has gone very wrong.
                panic!();
            }
            GatewaySpComponent::ROT => {
                // We're dealing with RoT targets at this point.
                match self.sprot.reset() {
                    Err(SprotError::Protocol(SprotProtocolError::Timeout)) => {
                        // This is the expected error if the reset was successful.
                        // It could be that the RoT is out-to-lunch for some other
                        // reason though.
                        // Things for upper layers to do:
                        //   - Check a boot nonce type thing to see if we are in a
                        //     new session.
                        //   - Check that the expected image is now running.
                        //     (Management plane should do that.)
                        //   - Enable staged updates where we don't automatically
                        //     reset after writing an image.
                        ringbuf_entry!(Log::RotReset(
                            SprotProtocolError::Timeout.into()
                        ));
                        Ok(())
                    }
                    Err(err) => {
                        // Some other error occurred.
                        // Update is all-or-nothing at the moment.
                        // The control plane can try to reset the RoT again or it
                        // can start the update process all over again.  We should
                        // be able to make incremental progress if there is some
                        // bug/condition that is degrading SpRot communications.

                        ringbuf_entry!(Log::RotReset(err));
                        Err(err.into())
                    }
                    Ok(()) => {
                        ringbuf_entry!(Log::ExpectedRspTimeout);
                        Ok(())
                    }
                }
            }
            // mgs_{gimlet,psc,sidecar}.rs deal with any board specific
            // reset strategy. Here we take care of common SP and RoT cases.
            _ => Err(GatewaySpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_get_active_slot(
        &mut self,
        component: GatewaySpComponent,
    ) -> Result<u16, GatewaySpError> {
        match component {
            GatewaySpComponent::ROT => {
                let slot = match self.sprot.rot_boot_info()?.active {
                    SpSlotId::A => 0,
                    SpSlotId::B => 1,
                };
                Ok(slot)
            }
            _ => Err(GatewaySpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_set_active_slot(
        &mut self,
        component: GatewaySpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), GatewaySpError> {
        match component {
            GatewaySpComponent::ROT => {
                let slot = slot.try_into().map_err(|()| {
                    GatewaySpError::RequestUnsupportedForComponent
                })?;
                let duration = if persist {
                    SwitchDuration::Forever
                } else {
                    SwitchDuration::Once
                };
                self.sprot.switch_default_image(slot, duration)?;
                Ok(())
            }

            GatewaySpComponent::STAGE0 => {
                let slot = slot.try_into().map_err(|()| {
                    GatewaySpError::RequestUnsupportedForComponent
                })?;
                let duration = if persist {
                    SwitchDuration::Forever
                } else {
                    SwitchDuration::Once
                };
                self.sprot.component_switch_default_image(
                    SpRotComponent::Stage0,
                    slot,
                    duration,
                )?;
                Ok(())
            }

            // GatewaySpComponent::SP_ITSELF:
            // update_server for SP needs to decouple finish_update()
            // from swap_banks() for SwitchDuration::Forever to make sense.
            // There isn't currently a mechanism implemented for SP that
            // enables SwitchDuration::Once.
            //
            // Other components might also be served someday.
            _ => Err(GatewaySpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn read_sensor(
        &mut self,
        req: SensorRequest,
    ) -> Result<SensorResponse, GatewaySpError> {
        let id = SensorId(req.id);
        match req.kind {
            SensorRequestKind::ErrorCount => {
                self.sensor.get_nerrors(id).map(SensorResponse::ErrorCount)
            }
            SensorRequestKind::LastReading => {
                self.sensor.get_raw_reading(id).map(|r| match r {
                    (Ok(value), timestamp) => {
                        SensorResponse::LastReading(SensorReading {
                            value: Ok(value),
                            timestamp,
                        })
                    }
                    (Err(nodata), timestamp) => {
                        SensorResponse::LastReading(SensorReading {
                            value: Err(translate_sensor_nodata(nodata)),
                            timestamp,
                        })
                    }
                })
            }
            SensorRequestKind::LastData => {
                self.sensor.get_last_data(id).map(|(value, timestamp)| {
                    SensorResponse::LastData { value, timestamp }
                })
            }
            SensorRequestKind::LastError => self
                .sensor
                .get_last_nodata(id)
                .map(|(nodata, timestamp)| SensorResponse::LastError {
                    value: translate_sensor_nodata(nodata),
                    timestamp,
                }),
        }
        .map_err(|e| {
            use gateway_messages::SensorError;
            use task_sensor_api::SensorApiError;
            GatewaySpError::Sensor(match e {
                SensorApiError::InvalidSensor => SensorError::InvalidSensor,
                SensorApiError::NoReading => SensorError::NoReading,
            })
        })
    }

    pub(crate) fn current_time(&mut self) -> Result<u64, GatewaySpError> {
        Ok(sys_get_timer().now)
    }

    pub(crate) fn read_rot_page(
        &mut self,
        req: RotRequest,
        buf: &mut [u8],
    ) -> Result<RotResponse, GatewaySpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ReadRotPage));
        let page = match req {
            RotRequest::ReadCmpa => drv_sprot_api::RotPage::Cmpa,
            RotRequest::ReadCfpa(CfpaPage::Scratch) => {
                drv_sprot_api::RotPage::CfpaScratch
            }
            RotRequest::ReadCfpa(CfpaPage::Active) => {
                drv_sprot_api::RotPage::CfpaActive
            }
            RotRequest::ReadCfpa(CfpaPage::Inactive) => {
                drv_sprot_api::RotPage::CfpaInactive
            }
        };

        match self
            .sprot
            .read_rot_page(page, &mut buf[..lpc55_rom_data::FLASH_PAGE_SIZE])
        {
            Ok(_) => Ok(RotResponse::Ok),
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(not(feature = "vpd"))]
    pub(crate) fn vpd_lock_status_all(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, GatewaySpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::VpdLockStatus));
        Err(GatewaySpError::Vpd(GatewayVpdError::NotImplemented))
    }

    #[cfg(feature = "vpd")]
    pub(crate) fn vpd_lock_status_all(
        &self,
        buf: &mut [u8],
    ) -> Result<usize, GatewaySpError> {
        use task_vpd_api::{Vpd, VpdError};
        task_slot!(VPD, vpd);

        ringbuf_entry!(Log::MgsMessage(MgsMessage::VpdLockStatus));
        let vpd = Vpd::from(VPD.get_task_id());
        let cnt = vpd.num_vpd_devices();

        for (i, entry) in buf.iter_mut().enumerate().take(cnt) {
            // `cnt` is based on the static size of the number of VPD devices.
            // All the VPD APIs work off of a `u8` so if the number of VPD
            // devices is returned as being greater than a `u8` we wouldn't
            // actually be able to access them. We could probably remove this
            // panic...
            let idx = match u8::try_from(i) {
                Ok(v) => v,
                Err(_) => panic!(),
            };
            *entry = match vpd.is_locked(idx) {
                Ok(v) => v.into(),
                Err(e) => {
                    return Err(GatewaySpError::Vpd(match e {
                        VpdError::InvalidDevice => {
                            GatewayVpdError::InvalidDevice
                        }
                        VpdError::NotPresent => GatewayVpdError::NotPresent,
                        VpdError::DeviceError => GatewayVpdError::DeviceError,
                        VpdError::Unavailable => GatewayVpdError::Unavailable,
                        VpdError::DeviceTimeout => {
                            GatewayVpdError::DeviceTimeout
                        }
                        VpdError::DeviceOff => GatewayVpdError::DeviceOff,
                        VpdError::BadAddress => GatewayVpdError::BadAddress,
                        VpdError::BadBuffer => GatewayVpdError::BadBuffer,
                        VpdError::BadRead => GatewayVpdError::BadRead,
                        VpdError::BadWrite => GatewayVpdError::BadWrite,
                        VpdError::BadLock => GatewayVpdError::BadLock,
                        VpdError::NotImplemented => {
                            GatewayVpdError::NotImplemented
                        }
                        VpdError::IsLocked => GatewayVpdError::IsLocked,
                        VpdError::PartiallyLocked => {
                            GatewayVpdError::PartiallyLocked
                        }
                        VpdError::AlreadyLocked => {
                            GatewayVpdError::AlreadyLocked
                        }
                        VpdError::ServerRestarted => {
                            GatewayVpdError::TaskRestarted
                        }
                    }))
                }
            }
        }

        Ok(cnt)
    }
    pub(crate) fn versioned_rot_boot_info(
        &mut self,
        version: u8,
    ) -> Result<GatewayRotBootInfo, GatewaySpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::VersionedRotBootInfo {
            version
        }));

        match self.sprot.versioned_rot_boot_info(version)? {
            SpVersionedRotBootInfo::V1(v1) => {
                ringbuf_entry!(Log::N(version));
                Ok(GatewayRotBootInfo::V1(MgsRotState::from(v1).0))
            }
            SpVersionedRotBootInfo::V2(v2) => match version {
                2 => {
                    ringbuf_entry!(Log::N(version));
                    Ok(GatewayRotBootInfo::V2(MgsRotStateV2::from(v2).0))
                }
                _ => {
                    ringbuf_entry!(Log::N(version));
                    Ok(GatewayRotBootInfo::V3(MgsRotStateV3::from(v2).0))
                } // RoT's V2 is MGS V3 and the highest version that
                  // we can offer today.
            },
            // New variants that this code doesn't know about yet will
            // result in a deserialization error.
        }
    }
}

fn translate_sensor_nodata(
    n: task_sensor_api::NoData,
) -> gateway_messages::SensorDataMissing {
    use gateway_messages::SensorDataMissing;
    use task_sensor_api::NoData;
    match n {
        NoData::DeviceOff => SensorDataMissing::DeviceOff,
        NoData::DeviceError => SensorDataMissing::DeviceError,
        NoData::DeviceNotPresent => SensorDataMissing::DeviceNotPresent,
        NoData::DeviceUnavailable => SensorDataMissing::DeviceUnavailable,
        NoData::DeviceTimeout => SensorDataMissing::DeviceTimeout,
    }
}

struct MgsFwid(GatewayFwid);
impl From<SpFwid> for MgsFwid {
    fn from(fwid: SpFwid) -> MgsFwid {
        MgsFwid(match fwid {
            SpFwid::Sha3_256(digest) => GatewayFwid::Sha3_256(digest),
        })
    }
}

struct MgsRotSlotId(GatewayRotSlotId);
impl From<SpSlotId> for MgsRotSlotId {
    // This is use to convert an external input from the
    // SpRot connection. What happens if a newer RoT image gives us
    // something we don't yet know about?
    fn from(id: SpSlotId) -> MgsRotSlotId {
        MgsRotSlotId(match id {
            SpSlotId::A => GatewayRotSlotId::A,
            SpSlotId::B => GatewayRotSlotId::B,
        })
    }
}

struct MgsRotImageDetails(GatewayRotImageDetails);
impl From<SpRotImageDetails> for MgsRotImageDetails {
    fn from(details: SpRotImageDetails) -> MgsRotImageDetails {
        MgsRotImageDetails(GatewayRotImageDetails {
            digest: details.digest,
            version: MgsImageVersion::from(details.version).0,
        })
    }
}

struct MgsRotState(GatewayRotState);

impl From<SpRotBootInfo> for MgsRotState {
    fn from(v1: SpRotBootInfo) -> MgsRotState {
        MgsRotState(GatewayRotState {
            rot_updates: GatewayRotUpdateDetails {
                boot_state: GatewayRotBootState {
                    active: MgsRotSlotId::from(v1.active).0,
                    slot_a: v1.slot_a_sha3_256_digest.map(|digest| {
                        GatewayRotImageDetails {
                            version: MgsImageVersion::from(SpImageVersion {
                                version: 0,
                                epoch: 0,
                            })
                            .0,
                            digest,
                        }
                    }),
                    slot_b: v1.slot_b_sha3_256_digest.map(|digest| {
                        GatewayRotImageDetails {
                            version: MgsImageVersion::from(SpImageVersion {
                                version: 0,
                                epoch: 0,
                            })
                            .0,
                            digest,
                        }
                    }),
                },
            },
        })
    }
}

impl From<SpRotBootInfo> for MgsRotStateV2 {
    fn from(boot_info: SpRotBootInfo) -> MgsRotStateV2 {
        MgsRotStateV2(GatewayRotStateV2 {
            active: MgsRotSlotId::from(boot_info.active).0,
            persistent_boot_preference: MgsRotSlotId::from(
                boot_info.persistent_boot_preference,
            )
            .0,
            pending_persistent_boot_preference: boot_info
                .pending_persistent_boot_preference
                .map(|id| MgsRotSlotId::from(id).0),
            transient_boot_preference: boot_info
                .transient_boot_preference
                .map(|id| MgsRotSlotId::from(id).0),
            slot_a_sha3_256_digest: boot_info.slot_a_sha3_256_digest,
            slot_b_sha3_256_digest: boot_info.slot_b_sha3_256_digest,
        })
    }
}

struct MgsRotStateV2(GatewayRotStateV2);

impl From<SpRotBootInfoV2> for MgsRotStateV2 {
    fn from(boot_info: SpRotBootInfoV2) -> MgsRotStateV2 {
        MgsRotStateV2(GatewayRotStateV2 {
            active: MgsRotSlotId::from(boot_info.active).0,
            persistent_boot_preference: MgsRotSlotId::from(
                boot_info.persistent_boot_preference,
            )
            .0,
            pending_persistent_boot_preference: boot_info
                .pending_persistent_boot_preference
                .map(|id| MgsRotSlotId::from(id).0),
            transient_boot_preference: boot_info
                .transient_boot_preference
                .map(|id| MgsRotSlotId::from(id).0),
            slot_a_sha3_256_digest: match boot_info.slot_a_status {
                Ok(_) => {
                    let SpFwid::Sha3_256(digest) = boot_info.slot_a_fwid;
                    Some(digest)
                }
                Err(_) => None,
            },
            slot_b_sha3_256_digest: match boot_info.slot_b_status {
                Ok(_) => {
                    let SpFwid::Sha3_256(digest) = boot_info.slot_b_fwid;
                    Some(digest)
                }
                Err(_) => None,
            },
        })
    }
}

struct MgsImageVersion(GatewayImageVersion);

impl From<SpImageVersion> for MgsImageVersion {
    fn from(iv: SpImageVersion) -> Self {
        MgsImageVersion(GatewayImageVersion {
            version: iv.version,
            epoch: iv.epoch,
        })
    }
}

struct MgsRotStateV3(GatewayRotStateV3);

impl From<SpRotBootInfoV2> for MgsRotStateV3 {
    fn from(boot_info: SpRotBootInfoV2) -> MgsRotStateV3 {
        MgsRotStateV3(GatewayRotStateV3 {
            active: MgsRotSlotId::from(boot_info.active).0,
            persistent_boot_preference: MgsRotSlotId::from(
                boot_info.persistent_boot_preference,
            )
            .0,
            pending_persistent_boot_preference: boot_info
                .pending_persistent_boot_preference
                .map(|s| MgsRotSlotId::from(s).0),
            transient_boot_preference: boot_info
                .transient_boot_preference
                .map(|s| MgsRotSlotId::from(s).0),
            slot_a_fwid: MgsFwid::from(boot_info.slot_a_fwid).0,
            slot_b_fwid: MgsFwid::from(boot_info.slot_b_fwid).0,
            stage0_fwid: MgsFwid::from(boot_info.stage0_fwid).0,
            stage0next_fwid: MgsFwid::from(boot_info.stage0next_fwid).0,
            slot_a_status: boot_info
                .slot_a_status
                .map_err(|e| MgsImageError::from(e).0),
            slot_b_status: boot_info
                .slot_b_status
                .map_err(|e| MgsImageError::from(e).0),
            stage0_status: boot_info
                .stage0_status
                .map_err(|e| MgsImageError::from(e).0),
            stage0next_status: boot_info
                .stage0next_status
                .map_err(|e| MgsImageError::from(e).0),
        })
    }
}

struct MgsImageError(GatewayImageError);
impl From<SpImageError> for MgsImageError {
    fn from(ie: SpImageError) -> MgsImageError {
        MgsImageError(match ie {
            SpImageError::Unchecked => GatewayImageError::Unchecked,
            SpImageError::FirstPageErased => GatewayImageError::FirstPageErased,
            SpImageError::PartiallyProgrammed => {
                GatewayImageError::PartiallyProgrammed
            }
            SpImageError::InvalidLength => GatewayImageError::InvalidLength,
            SpImageError::HeaderNotProgrammed => {
                GatewayImageError::HeaderNotProgrammed
            }
            SpImageError::BootloaderTooSmall => {
                GatewayImageError::BootloaderTooSmall
            }
            SpImageError::BadMagic => GatewayImageError::BadMagic,
            SpImageError::HeaderImageSize => GatewayImageError::HeaderImageSize,
            SpImageError::UnalignedLength => GatewayImageError::UnalignedLength,
            SpImageError::UnsupportedType => GatewayImageError::UnsupportedType,
            SpImageError::ResetVectorNotThumb2 => {
                GatewayImageError::ResetVectorNotThumb2
            }
            SpImageError::ResetVector => GatewayImageError::ResetVector,
            SpImageError::Signature => GatewayImageError::Signature,
        })
    }
}
