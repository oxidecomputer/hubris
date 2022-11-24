// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, update::sp::SpUpdate, Log, MgsMessage};
use core::convert::Infallible;
use drv_sprot_api::SpRot;
use gateway_messages::{
    DiscoverResponse, ImageVersion, PowerState, RotError, RotState, SpError,
    SpPort, SpState,
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

fn rot_state(sprot: &SpRot) -> Result<RotState, RotError> {
    let status = sprot.status().map_err(SprotErrorConvert)?;
    Ok(RotState {
        version: ImageVersion {
            version: status.version,
            epoch: status.epoch,
        },
        messages_received: status.rx_received,
        invalid_messages_received: status.rx_invalid,
        incomplete_transmissions: status.tx_incomplete,
        rx_fifo_overrun: status.rx_overrun,
        tx_fifo_underrun: status.tx_underrun,
    })
}

pub(crate) struct SprotErrorConvert(pub drv_sprot_api::SprotError);

impl From<SprotErrorConvert> for RotError {
    fn from(err: SprotErrorConvert) -> Self {
        RotError::MessageError { code: err.0 as u32 }
    }
}
