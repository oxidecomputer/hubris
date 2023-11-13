// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, Log, MgsMessage};
use drv_caboose::{CabooseError, CabooseReader};
use drv_sprot_api::{
    CabooseOrSprotError, ImageError as SpImageError,
    ImageVersion as SpImageVersion, RotBootInfo as SpRotBootInfo,
    RotBootInfoV2 as SpRotBootInfoV2, RotState as SpRotState,
    SlotId as SpSlotId, SpRot, SprotError, SprotProtocolError, SwitchDuration,
    VersionedRotBootInfo as SpVersionedRotBootInfo,
};
use drv_stm32h7_update_api::Update;
use gateway_messages::{
    CfpaPage, DiscoverResponse, ImageError as GmImageError,
    ImageVersion as GmImageVersion, PowerState, RotError, RotRequest,
    RotResponse, RotSlotId as GmRotSlotId, RotStateV2 as GmRotStateV2,
    RotStateV3 as GmRotStateV3, SensorReading, SensorRequest,
    SensorRequestKind, SensorResponse, SpComponent as GmSpComponent,
    SpError as GmSpError, SpPort as GmSpPort, SpStateV2 as GmSpStateV2,
    SpStateV3 as GmSpStateV3, VersionedRotState as GmVersionedRotState,
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
    reset_component_requested: Option<GmSpComponent>,
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
        port: GmSpPort,
    ) -> Result<DiscoverResponse, GmSpError> {
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
    ) -> Result<GmSpStateV2, GmSpError> {
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

        let mut state = GmSpStateV2 {
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
        component: GmSpComponent,
        slot: u16,
        key: [u8; 4],
        buf: &mut [u8],
    ) -> Result<usize, GmSpError> {
        let caboose_to_sp_error = |e| {
            match e {
                CabooseError::NoSuchTag => GmSpError::NoSuchCabooseKey(key),
                CabooseError::MissingCaboose => GmSpError::NoCaboose,
                CabooseError::BadChecksum => GmSpError::BadCabooseChecksum,
                CabooseError::TlvcReaderBeginFailed
                | CabooseError::RawReadFailed
                | CabooseError::InvalidRead
                | CabooseError::TlvcReadExactFailed => {
                    GmSpError::CabooseReadError
                }

                // NoImageHeader is only returned when reading the caboose of the
                // bank2 slot; it shouldn't ever be returned by the local reader.
                CabooseError::NoImageHeader => GmSpError::NoCaboose,
            }
        };
        let caboose_or_sprot_to_sp_error = |e| match e {
            CabooseOrSprotError::Caboose(e) => caboose_to_sp_error(e),
            CabooseOrSprotError::Sprot(e) => e.into(),
        };

        match component {
            GmSpComponent::SP_ITSELF => match slot {
                0 => {
                    let reader = drv_caboose_pos::CABOOSE_POS
                        .as_slice()
                        .map(CabooseReader::new)
                        .ok_or(GmSpError::NoCaboose)?;
                    let v = reader.get(key).map_err(caboose_to_sp_error)?;
                    let len = v.len();
                    if len > buf.len() {
                        Err(GmSpError::CabooseValueOverflow(len as u32))
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
                _ => Err(GmSpError::InvalidSlotForComponent),
            },
            GmSpComponent::ROT => {
                let slot_id = slot
                    .try_into()
                    .map_err(|()| GmSpError::InvalidSlotForComponent)?;
                let len = self
                    .sprot
                    .read_caboose_value(slot_id, key, buf)
                    .map_err(caboose_or_sprot_to_sp_error)?;
                Ok(len as usize)
            }
            _ => Err(GmSpError::RequestUnsupportedForComponent),
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
        component: GmSpComponent,
    ) -> Result<(), GmSpError> {
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
        component: GmSpComponent,
    ) -> Result<(), GmSpError> {
        if self.reset_component_requested != Some(component) {
            return Err(GmSpError::ResetComponentTriggerWithoutPrepare);
        }
        // If we are not resetting the SP_ITSELF, then we may come back here
        // to reset something else or to run another prepare/trigger on
        // the same component.
        self.reset_component_requested = None;

        // Resetting the SP through reset_component() is
        // the same as through reset() until transient bank selection is
        // figured out for the SP.
        match component {
            GmSpComponent::SP_ITSELF => {
                task_jefe_api::Jefe::from(crate::JEFE.get_task_id())
                    .request_reset();
                // If `request_reset()` returns,
                // something has gone very wrong.
                panic!();
            }
            GmSpComponent::ROT => {
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
            _ => Err(GmSpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_get_active_slot(
        &mut self,
        component: GmSpComponent,
    ) -> Result<u16, GmSpError> {
        match component {
            GmSpComponent::ROT => {
                let SpRotState::V1 { state, .. } = self.sprot.rot_state()?;
                let slot = match state.active {
                    drv_sprot_api::RotSlot::A => 0,
                    drv_sprot_api::RotSlot::B => 1,
                    drv_sprot_api::RotSlot::Stage0 => 2,
                    drv_sprot_api::RotSlot::Stage0Next => 3,
                };
                Ok(slot)
            }
            _ => Err(GmSpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_set_active_slot(
        &mut self,
        component: GmSpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), GmSpError> {
        match component {
            GmSpComponent::ROT => {
                let slot = slot
                    .try_into()
                    .map_err(|()| GmSpError::RequestUnsupportedForComponent)?;
                let duration = if persist {
                    SwitchDuration::Forever
                } else {
                    SwitchDuration::Once
                };
                self.sprot.switch_default_image(slot, duration)?;
                Ok(())
            }

            // GmSpComponent::SP_ITSELF:
            // update_server for SP needs to decouple finish_update()
            // from swap_banks() for SwitchDuration::Forever to make sense.
            // There isn't currently a mechanism implemented for SP that
            // enables SwitchDuration::Once.
            //
            // Other components might also be served someday.
            _ => Err(GmSpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn read_sensor(
        &mut self,
        req: SensorRequest,
    ) -> Result<SensorResponse, GmSpError> {
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
            GmSpError::Sensor(match e {
                SensorApiError::InvalidSensor => SensorError::InvalidSensor,
                SensorApiError::NoReading => SensorError::NoReading,
            })
        })
    }

    pub(crate) fn current_time(&mut self) -> Result<u64, GmSpError> {
        Ok(sys_get_timer().now)
    }

    pub(crate) fn read_rot_page(
        &mut self,
        req: RotRequest,
        buf: &mut [u8],
    ) -> Result<RotResponse, GmSpError> {
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

    pub(crate) fn sp_state_rot_version(
        &mut self,
        power_state: PowerState,
        version: u8,
    ) -> Result<GmSpStateV3, GmSpError> {
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

        let mut state = GmSpStateV3 {
            serial_number: [0; SP_STATE_FIELD_WIDTH],
            model: [0; SP_STATE_FIELD_WIDTH],
            revision: id.revision,
            hubris_archive_id: kipc::read_image_id().to_le_bytes(),
            base_mac_address: self.base_mac_address.0,
            power_state,
            rot: self
                .sprot
                .versioned_rot_boot_info(version)
                .map(|s| MgsVersionedRotState::from(s).0)
                .map_err(RotError::from),
            // Ok(VersionedRotState)
            // Err(GmRotError)
            // self.sprot.versioned_rot_boot_info(version), // versioned_rot_boot_info(&self.sprot, version),
            // 448 |                 .map(|s| GmVersionedRotState::from(s))
            //     |                          ------------------------- ^ expected enum
            //     `VersionedRotState`, found enum `VersionedRotBootInfo`
        };

        let n = usize::min(state.serial_number.len(), id.serial.len());
        state.serial_number[..n].copy_from_slice(&id.serial);

        let n = usize::min(state.model.len(), id.part_number.len());
        state.model[..n].copy_from_slice(&id.part_number);

        Ok(state)
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

struct MgsRotSlotId(GmRotSlotId);
impl From<SpSlotId> for MgsRotSlotId {
    // This is use to convert an external input from the
    // SpRot connection. What happens if a newer RoT image gives us
    // something we don't yet know about?
    fn from(id: SpSlotId) -> MgsRotSlotId {
        MgsRotSlotId(match id {
            SpSlotId::A => GmRotSlotId::A,
            SpSlotId::B => GmRotSlotId::B,
            SpSlotId::Stage0 => GmRotSlotId::Stage0,
            SpSlotId::Stage0Next => GmRotSlotId::Stage0Next,
        })
    }
}

/*
struct MgsRotState(GmRotState);

impl From<SpRotState> for MgsRotState {
    fn from(state: SpRotState) -> MgsRotState {
        match state {
            SpRotState::V1(v1_state) => MgsRotState(GmRotState {
                rot_updates: GmRotUpdateDetails {
                    boot_state: GmRotBootState {
                        active: v1_state.active,
                        slot_a: v1_state.slot_a,
                        slot_b: v1_state.slot_b,
                    },
                },
            }),
        }
    }
}
*/

/*
impl From<SpRotBootInfo> for MgsRotState {
    fn from(boot_info: SpRotBootInfo) -> MgsRotStateV2 {
        let convert_slot_details = |digest: Option<[u8; 32]>| {
            digest.map(|d| gateway_messages::RotImageDetails {
                digest: d,
                version: gateway_messages::ImageVersion {
                    version: 0,
                    epoch: 0,
                },
            })
        };

        MgsRotState(GmRotState {
            rot_updates: gateway_messages::RotUpdateDetails {
                boot_state: gateway_messages::RotBootState {
                    active: MgsRotSlotId::from(boot_info.active).0,
                    slot_a: convert_slot_details(
                        boot_info.slot_a_sha3_256_digest,
                    ),
                    slot_b: convert_slot_details(
                        boot_info.slot_b_sha3_256_digest,
                    ),
                },
            },
        })
    }
}
*/

impl From<SpRotBootInfo> for MgsRotStateV2 {
    fn from(boot_info: SpRotBootInfo) -> MgsRotStateV2 {
        MgsRotStateV2(GmRotStateV2 {
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

struct MgsVersionedRotState(GmVersionedRotState);

impl From<SpVersionedRotBootInfo> for MgsVersionedRotState {
    fn from(boot_info: SpVersionedRotBootInfo) -> MgsVersionedRotState {
        match boot_info {
            SpVersionedRotBootInfo::V1(state) => {
                MgsVersionedRotState(GmVersionedRotState::V2(GmRotStateV2 {
                    active: MgsRotSlotId::from(state.active).0,
                    persistent_boot_preference: MgsRotSlotId::from(
                        state.persistent_boot_preference,
                    )
                    .0,
                    pending_persistent_boot_preference: state
                        .pending_persistent_boot_preference
                        .map(|id| MgsRotSlotId::from(id).0),
                    transient_boot_preference: state
                        .transient_boot_preference
                        .map(|id| MgsRotSlotId::from(id).0),
                    slot_a_sha3_256_digest: state.slot_a_sha3_256_digest,
                    slot_b_sha3_256_digest: state.slot_b_sha3_256_digest,
                }))
            }
            SpVersionedRotBootInfo::V2(state) => {
                MgsVersionedRotState(GmVersionedRotState::V3(GmRotStateV3 {
                    active: MgsRotSlotId::from(state.active).0,
                    persistent_boot_preference: MgsRotSlotId::from(
                        state.persistent_boot_preference,
                    )
                    .0,
                    pending_persistent_boot_preference: state
                        .pending_persistent_boot_preference
                        .map(|id| MgsRotSlotId::from(id).0),
                    transient_boot_preference: state
                        .transient_boot_preference
                        .map(|id| MgsRotSlotId::from(id).0),
                    slot_a_sha3_256_digest: state.slot_a_sha3_256_digest,
                    slot_b_sha3_256_digest: state.slot_b_sha3_256_digest,
                    stage0_sha3_256_digest: state.stage0_sha3_256_digest,
                    stage0_next_sha3_256_digest: state
                        .stage0_next_sha3_256_digest,

                    slot_a_status: state
                        .slot_a_status
                        .map(|s| MgsImageVersion::from(s).0)
                        .map_err(|e| MgsImageError::from(e).0),
                    slot_b_status: state
                        .slot_b_status
                        .map(|s| MgsImageVersion::from(s).0)
                        .map_err(|e| MgsImageError::from(e).0),
                    stage0_status: state
                        .stage0_status
                        .map(|s| MgsImageVersion::from(s).0)
                        .map_err(|e| MgsImageError::from(e).0),
                    stage0_next_status: state
                        .stage0_next_status
                        .map(|s| MgsImageVersion::from(s).0)
                        .map_err(|e| MgsImageError::from(e).0),
                }))
            }
        }
    }
}

struct MgsRotStateV2(GmRotStateV2);

impl From<SpRotBootInfoV2> for MgsRotStateV2 {
    fn from(boot_info: SpRotBootInfoV2) -> MgsRotStateV2 {
        MgsRotStateV2(GmRotStateV2 {
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
                Ok(_) => Some(boot_info.slot_a_sha3_256_digest),
                Err(_) => None,
            },
            slot_b_sha3_256_digest: match boot_info.slot_b_status {
                Ok(_) => Some(boot_info.slot_b_sha3_256_digest),
                Err(_) => None,
            },
        })
    }
}

struct MgsImageVersion(GmImageVersion);

impl From<SpImageVersion> for MgsImageVersion {
    fn from(iv: SpImageVersion) -> Self {
        MgsImageVersion(GmImageVersion {
            version: iv.version,
            epoch: iv.epoch,
        })
    }
}

struct MgsRotStateV3(GmRotStateV3);

impl From<SpRotBootInfoV2> for MgsRotStateV3 {
    fn from(boot_info: SpRotBootInfoV2) -> MgsRotStateV3 {
        MgsRotStateV3(GmRotStateV3 {
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
            slot_a_sha3_256_digest: boot_info.slot_a_sha3_256_digest,
            slot_b_sha3_256_digest: boot_info.slot_b_sha3_256_digest,
            stage0_sha3_256_digest: boot_info.stage0_sha3_256_digest,
            stage0_next_sha3_256_digest: boot_info.stage0_next_sha3_256_digest,
            slot_a_status: boot_info
                .slot_a_status
                .map(|s| MgsImageVersion::from(s).0)
                .map_err(|e| MgsImageError::from(e).0),
            slot_b_status: boot_info
                .slot_b_status
                .map(|s| MgsImageVersion::from(s).0)
                .map_err(|e| MgsImageError::from(e).0),
            stage0_status: boot_info
                .stage0_status
                .map(|s| MgsImageVersion::from(s).0)
                .map_err(|e| MgsImageError::from(e).0),
            stage0_next_status: boot_info
                .stage0_next_status
                .map(|s| MgsImageVersion::from(s).0)
                .map_err(|e| MgsImageError::from(e).0),
        })
    }
}

struct MgsImageError(GmImageError);
impl From<SpImageError> for MgsImageError {
    fn from(ie: SpImageError) -> MgsImageError {
        MgsImageError(match ie {
            SpImageError::Unchecked => GmImageError::Unchecked,
            SpImageError::FirstPageErased => GmImageError::FirstPageErased,
            SpImageError::PartiallyProgrammed => {
                GmImageError::PartiallyProgrammed
            }
            SpImageError::InvalidLength => GmImageError::InvalidLength,
            SpImageError::HeaderNotProgrammed => {
                GmImageError::HeaderNotProgrammed
            }
            SpImageError::BootloaderTooSmall => {
                GmImageError::BootloaderTooSmall
            }
            SpImageError::BadMagic => GmImageError::BadMagic,
            SpImageError::HeaderImageSize => GmImageError::HeaderImageSize,
            SpImageError::UnalignedLength => GmImageError::UnalignedLength,
            SpImageError::UnsupportedType => GmImageError::UnsupportedType,
            SpImageError::ResetVectorNotThumb2 => {
                GmImageError::ResetVectorNotThumb2
            }
            SpImageError::ResetVector => GmImageError::ResetVector,
            SpImageError::Signature => GmImageError::Signature,
        })
    }
}

/*
fn versioned_rot_boot_info(
    sprot: &SpRot,
    version: u8,
) -> Result<SpVersionedRotState, RotError> {
    match sprot.versioned_rot_boot_info(version)? {
        drv_sprot_api::VersionedRotBootInfo::V1(boot_info) => {
            Ok(VersionedRotState::V1(MgsRotState::from(boot_info).0))
        }

        drv_sprot_api::VersionedRotBootInfo::V2(boot_info) => {
            Ok(VersionedRotState::V2(MgsRotStateV2::from(boot_info).0))
        }
    }
}

// Conversion between gateway_messages types and hubris types is tedious and
// confusing because of the similar names and purposes in the Rot handoff
// area, sprot protocol, and MGS structures.

fn rot_state_verison(
    sprot: &SpRot,
    version: u8,
) -> Result<VersionedRotState, RotError> {
    match sprot.versioned_rot_boot_info(version) {
        SpVersionedRotBootInfo::V1(boot_info) => {
            Ok(gateway_messages::VersionedRotState::V2(RotStateV2 {
                active: MgsRotSlotId::from(boot_info.active).0,
                persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                pending_persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                transient_boot_preference: boot_info
                    .transient_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                slot_a_sha3_256_digest: boot_info.slot_a_sha3_256_digest,
                slot_b_sha3_256_digest: boot_info.slot_b_sha3_256_digest,
            }))
        }
        SpVersionedRotBootInfo::V2(boot_info) => {
            Ok(gateway_messages::VersionedRotState::V3(RotStateV3 {
                active: MgsRotSlotId::from(boot_info.active).0,
                persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                pending_persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                transient_boot_preference: boot_info
                    .transient_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),

                slot_a_sha3_256_digest: boot_info.slot_a_sha3_256_digest,
                slot_b_sha3_256_digest: boot_info.slot_b_sha3_256_digest,
                stage0_sha3_256_digest: boot_info.stage0_sha3_256_digest,
                stage0_next_sha3_256_digest: boot_info
                    .stage0_next_sha3_256_digest,
                // Result<ImageVersion, ImageError>,
                slot_a_status: boot_info.slot_a_status,
                slot_b_status: boot_info.slot_b_status,
                stage0_status: boot_info.stage0_status,
                stage0_next_status: boot_info.stage0_next_status,
            }))
        }
        gateway_messages::VersionedRotState::V2(boot_info) => {
            Ok(gateway_messages::VersionedRotState::V2(RotStateV3 {
                active: MgsRotSlotId::from(boot_info.active).0,
                persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                pending_persistent_boot_preference: boot_info
                    .persistent_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),
                transient_boot_preference: boot_info
                    .transient_boot_preference
                    .map(|s| MgsRotSlotId::from(s).0),

                slot_a_sha3_256_digest: boot_info.slot_a_sha3_256_digest,
                slot_b_sha3_256_digest: boot_info.slot_b_sha3_256_digest,
                stage0_sha3_256_digest: boot_info.stage0_sha3_256_digest,
                stage0_next_sha3_256_digest: boot_info
                    .stage0_next_sha3_256_digest,
                // Result<ImageVersion, ImageError>,
                slot_a_status: boot_info.slot_a_status,
                slot_b_status: boot_info.slot_b_status,
                stage0_status: boot_info.stage0_status,
                stage0_next_status: boot_info.stage0_next_status,
            }))
        }
        Error(e) => Err(e),
    }
}
*/
