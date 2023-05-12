// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, Log, MgsMessage};
use drv_caboose::{CabooseError, CabooseReader};
use drv_sprot_api::{
    RotState as SprotRotState, SlotId, SpRot, SprotError, SprotProtocolError,
    SwitchDuration,
};
use drv_stm32h7_update_api::Update;
use gateway_messages::{
    CabooseValue, DiscoverResponse, ImageVersion, PowerState, RotBootState,
    RotError, RotImageDetails, RotSlot, RotState, RotUpdateDetails,
    SpComponent, SpError, SpPort, SpState,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use static_assertions::const_assert;
use task_control_plane_agent_api::VpdIdentity;
use task_net_api::MacAddress;
use task_packrat_api::Packrat;
use userlib::{kipc, task_slot};

task_slot!(PACKRAT, packrat);
task_slot!(pub SPROT, sprot);
task_slot!(pub UPDATE_SERVER, update_server);

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    reset_component_requested: Option<SpComponent>,
    inventory: Inventory,
    base_mac_address: MacAddress,
    packrat: Packrat,
    sprot: SpRot,
    sp_update: Update,
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
        }
    }

    #[allow(dead_code)] // This function is only used by Gimlet right now
    pub(crate) fn packrat(&self) -> &Packrat {
        &self.packrat
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
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
        version: ImageVersion,
    ) -> Result<SpState, SpError> {
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

        let mut state = SpState {
            serial_number: [0; SP_STATE_FIELD_WIDTH],
            model: [0; SP_STATE_FIELD_WIDTH],
            revision: id.revision,
            hubris_archive_id: kipc::read_image_id().to_le_bytes(),
            base_mac_address: self.base_mac_address.0,
            version,
            power_state,
            rot: rot_state(&self.sprot),
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
        component: SpComponent,
        slot: u16,
        key: [u8; 4],
    ) -> Result<CabooseValue, SpError> {
        let r = match component {
            SpComponent::SP_ITSELF => match slot {
                0 => {
                    let reader = userlib::kipc::get_caboose()
                        .map(CabooseReader::new)
                        .ok_or(SpError::NoCaboose)?;
                    reader.get(key).map(CabooseValue::Local)
                }
                1 => self.sp_update.find_caboose_value(key).map(|pos| {
                    CabooseValue::Bank2 {
                        pos: pos.start..pos.end,
                    }
                }),
                _ => return Err(SpError::InvalidSlotForComponent),
            },
            SpComponent::ROT => {
                let slot_id = slot
                    .try_into()
                    .map_err(|()| SpError::InvalidSlotForComponent)?;
                RotCabooseReader::new(slot_id, &self.sprot)
                    .and_then(|r| r.get(key))
                    .map(|pos| CabooseValue::Rot { slot, pos })
            }
            _ => return Err(SpError::RequestUnsupportedForComponent),
        };
        r.map_err(|e| match e {
            CabooseError::NoSuchTag => SpError::NoSuchCabooseKey(key),
            CabooseError::MissingCaboose => SpError::NoCaboose,
            CabooseError::BadChecksum => SpError::BadCabooseChecksum,
            CabooseError::TlvcReaderBeginFailed
            | CabooseError::TlvcReadExactFailed
            | CabooseError::ReadFailed
            | CabooseError::InvalidRead => SpError::CabooseReadError,

            // NoImageHeader is only returned when reading the caboose of the
            // bank2 slot; it shouldn't ever be returned by the local reader.
            CabooseError::NoImageHeader => SpError::NoCaboose,
        })
    }

    pub fn copy_caboose_value_into(
        &self,
        value: CabooseValue,
        out: &mut [u8],
    ) -> Result<(), SpError> {
        match value {
            CabooseValue::Local(d) => {
                out.copy_from_slice(d);
            }
            CabooseValue::Bank2 { pos } => {
                self.sp_update
                    .read_raw_caboose(pos.start, out)
                    .map_err(|_| SpError::CabooseReadError)?;
            }
            CabooseValue::Rot { slot, pos } => {
                let slot = slot
                    .try_into()
                    .map_err(|()| SpError::InvalidSlotForComponent)?;
                self.sprot
                    .read_caboose_region(pos.start, slot, out)
                    .map_err(|_| SpError::CabooseReadError)?;
            }
        }
        Ok(())
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
        component: SpComponent,
    ) -> Result<(), SpError> {
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
        component: SpComponent,
    ) -> Result<(), SpError> {
        if self.reset_component_requested != Some(component) {
            return Err(SpError::ResetComponentTriggerWithoutPrepare);
        }
        // If we are not resetting the SP_ITSELF, then we may come back here
        // to reset something else or to run another prepare/trigger on
        // the same component.
        self.reset_component_requested = None;

        // Resetting the SP through reset_component() is
        // the same as through reset() until transient bank selection is
        // figured out for the SP.
        match component {
            SpComponent::SP_ITSELF => {
                task_jefe_api::Jefe::from(crate::JEFE.get_task_id())
                    .request_reset();
                // If `request_reset()` returns,
                // something has gone very wrong.
                panic!();
            }
            SpComponent::ROT => {
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
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_get_active_slot(
        &mut self,
        component: SpComponent,
    ) -> Result<u16, SpError> {
        match component {
            SpComponent::ROT => {
                let SprotRotState::V1 { state, .. } = self.sprot.rot_state()?;
                let slot = match state.active {
                    drv_sprot_api::RotSlot::A => 0,
                    drv_sprot_api::RotSlot::B => 1,
                };
                Ok(slot)
            }
            _ => return Err(SpError::RequestUnsupportedForComponent),
        }
    }

    pub(crate) fn component_set_active_slot(
        &mut self,
        component: SpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        match component {
            SpComponent::ROT => {
                let slot = slot
                    .try_into()
                    .map_err(|()| SpError::RequestUnsupportedForComponent)?;
                let duration = if persist {
                    SwitchDuration::Forever
                } else {
                    SwitchDuration::Once
                };
                self.sprot.switch_default_image(slot, duration)?;
                Ok(())
            }

            // SpComponent::SP_ITSELF:
            // update_server for SP needs to decouple finish_update()
            // from swap_banks() for SwitchDuration::Forever to make sense.
            // There isn't currently a mechanism implemented for SP that
            // enables SwitchDuration::Once.
            //
            // Other components might also be served someday.
            _ => return Err(SpError::RequestUnsupportedForComponent),
        }
    }
}

// conversion between gateway_messages types and hubris types is quite tedious.
fn rot_state(sprot: &SpRot) -> Result<RotState, RotError> {
    let SprotRotState::V1 { state, .. } = sprot.rot_state()?;
    let active = match state.active {
        drv_sprot_api::RotSlot::A => RotSlot::A,
        drv_sprot_api::RotSlot::B => RotSlot::B,
    };

    let slot_a = state.a.map(|a| RotImageDetailsConvert(a).into());
    let slot_b = state.b.map(|b| RotImageDetailsConvert(b).into());

    Ok(RotState {
        rot_updates: RotUpdateDetails {
            boot_state: RotBootState {
                active,
                slot_a,
                slot_b,
            },
        },
    })
}

pub(crate) struct RotImageDetailsConvert(
    pub drv_lpc55_update_api::RotImageDetails,
);

impl From<RotImageDetailsConvert> for RotImageDetails {
    fn from(value: RotImageDetailsConvert) -> Self {
        RotImageDetails {
            digest: value.0.digest,
            version: ImageVersion {
                epoch: value.0.version.epoch,
                version: value.0.version.version,
            },
        }
    }
}

#[derive(Copy, Clone)]
struct RotCabooseReader<'a> {
    sprot: &'a SpRot,
    size: u32,
    slot: SlotId,
}

impl<'a> RotCabooseReader<'a> {
    fn new(slot: SlotId, sprot: &'a SpRot) -> Result<Self, CabooseError> {
        let size = sprot.caboose_size(slot)?;
        ringbuf_entry!(Log::SprotCabooseSize(size));
        Ok(Self { size, slot, sprot })
    }

    pub fn get(
        &self,
        key: [u8; 4],
    ) -> Result<core::ops::Range<u32>, CabooseError> {
        let mut reader = tlvc::TlvcReader::begin(*self)
            .map_err(|_| CabooseError::TlvcReaderBeginFailed)?;
        while let Ok(Some(chunk)) = reader.next() {
            if chunk.header().tag == key {
                let mut tmp = [0u8; 32];
                if chunk.check_body_checksum(&mut tmp).is_err() {
                    return Err(CabooseError::BadChecksum);
                }
                // At this point, the reader is positioned **after** the data
                // from the target chunk.  We'll back up to the start of the
                // data slice.
                let (_reader, pos, _end) = reader.into_inner();

                let pos = pos as u32;
                let data_len = chunk.header().len.get();

                let data_start = pos
                    - chunk.header().total_len_in_bytes() as u32
                    + core::mem::size_of::<tlvc::ChunkHeader>() as u32;

                return Ok(data_start..(data_start + data_len));
            }
        }

        Err(CabooseError::NoSuchTag)
    }
}

impl tlvc::TlvcRead for RotCabooseReader<'_> {
    fn extent(&self) -> Result<u64, tlvc::TlvcReadError> {
        Ok(self.size as u64)
    }

    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), tlvc::TlvcReadError> {
        let offset = offset
            .try_into()
            .map_err(|_| tlvc::TlvcReadError::Truncated)?;
        ringbuf_entry!(Log::ReadCaboose(offset, dest.len()));
        if let Err(e) = self.sprot.read_caboose_region(offset, self.slot, dest)
        {
            ringbuf_entry!(Log::ReadCabooseErr(e));
            Err(tlvc::TlvcReadError::Truncated)
        } else {
            Ok(())
        }
    }
}
