// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, update::sp::SpUpdate, Log, MgsMessage};
use core::convert::Infallible;
use drv_sprot_api::SpRot;
use gateway_messages::{
    DiscoverResponse, ImageVersion, PowerState, RotBootState, RotError,
    RotImageDetails, RotSlot, RotState, RotUpdateDetails, SpError, SpPort,
    SpState,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    reset_requested: bool,
    inventory: Inventory,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            reset_requested: false,
            inventory: Inventory::new(),
        }
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn sp_state(
        &mut self,
        update: &SpUpdate,
        power_state: PowerState,
    ) -> Result<SpState, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

        // TODO Replace with the real serial number once it's available; for now
        // use the stm32 96-bit uid
        let mut serial_number = [0; 16];
        for (to, from) in serial_number.iter_mut().zip(
            drv_stm32xx_uid::read_uid()
                .iter()
                .flat_map(|x| x.to_be_bytes()),
        ) {
            *to = from;
        }

        Ok(SpState {
            serial_number,
            version: update.current_version(),
            power_state,
            rot: rot_state(update.sprot_task()),
        })
    }

    pub(crate) fn reset_prepare(&mut self) -> Result<(), SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ResetPrepare));
        self.reset_requested = true;
        Ok(())
    }

    pub(crate) fn reset_trigger(&mut self) -> Result<Infallible, SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        if !self.reset_requested {
            return Err(SpError::ResetTriggerWithoutPrepare);
        }

        let jefe = task_jefe_api::Jefe::from(crate::JEFE.get_task_id());
        jefe.request_reset();

        // If `request_reset()` returns, something has gone very wrong.
        panic!()
    }

    #[inline(always)]
    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }
}

// conversion between gateway_messages types and hubris types is quite tedious.
fn rot_state(sprot: &SpRot) -> Result<RotState, RotError> {
    let boot_state = sprot.status().map_err(SprotErrorConvert)?.rot_updates;
    let active = match boot_state.active {
        drv_update_api::RotSlot::A => RotSlot::A,
        drv_update_api::RotSlot::B => RotSlot::B,
    };

    let slot_a = boot_state.a.map(|a| RotImageDetailsConvert(a).into());
    let slot_b = boot_state.b.map(|b| RotImageDetailsConvert(b).into());

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

pub(crate) struct SprotErrorConvert(pub drv_sprot_api::SprotError);

impl From<SprotErrorConvert> for RotError {
    fn from(err: SprotErrorConvert) -> Self {
        RotError::MessageError { code: err.0 as u32 }
    }
}

pub(crate) struct RotImageDetailsConvert(pub drv_update_api::RotImageDetails);

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
