// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, Log, MgsMessage};
use core::convert::Infallible;
use gateway_messages::sp_impl::DeviceDescription;
use gateway_messages::{DiscoverResponse, SpError, SpPort, SpState};
use ringbuf::ringbuf_entry_root as ringbuf_entry;

// TODO How are we versioning SP images? This is a placeholder.
const VERSION: u32 = 1;

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

    pub(crate) fn sp_state(&mut self) -> Result<SpState, SpError> {
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
            version: VERSION,
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

    /// Number of devices returned in the inventory of this SP.
    pub(crate) fn inventory_num_devices(&self) -> u32 {
        self.inventory.num_devices() as u32
    }

    /// Get the description for the given device.
    ///
    /// This function should never fail, as the device inventory should be
    /// static. Acquiring the presence of a device may fail, but that should be
    /// indicated inline via the returned description's `presence` field.
    ///
    /// # Panics
    ///
    /// Implementors are allowed to panic if `index` is not in range (i.e., is
    /// greater than or equal to the value returned by `num_devices()`).
    pub(crate) fn inventory_device_description(
        &mut self,
        index: usize,
    ) -> DeviceDescription<'_> {
        self.inventory.device_description(index)
    }
}
