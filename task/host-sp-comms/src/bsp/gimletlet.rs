// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SP inventory types and implementation
//!
//! This reduces clutter in the main `ServerImpl` implementation
use super::ServerImpl;
use host_sp_messages::{InventoryData, InventoryDataResult};

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 1;

    pub(crate) fn perform_inventory_lookup(
        &mut self,
        sequence: u64,
        index: u32,
    ) -> Result<(), InventoryDataResult> {
        #[forbid(unreachable_patterns)]
        match index {
            0 => {
                // U12: the service processor itself
                // The UID is readable by stm32xx_sys
                let sys =
                    drv_stm32xx_sys_api::Sys::from(crate::SYS.get_task_id());
                let uid = sys.read_uid();
                let data = InventoryData::Stm32H7 {
                    uid,
                    dbgmcu_rev_id: 0, // TODO
                    dbgmcu_dev_id: 0, // TODO
                };

                self.tx_buf
                    .try_encode_inventory(sequence, b"U12", || Ok(&data));
            }

            // We need to specify INVENTORY_COUNT individually here to trigger
            // an error if we've overlapped it with a previous range
            Self::INVENTORY_COUNT | Self::INVENTORY_COUNT..=u32::MAX => {
                return Err(InventoryDataResult::InvalidIndex)
            }
        }
        Ok(())
    }
}
