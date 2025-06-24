// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SP inventory types and implementation
//!
//! This reduces clutter in the main `ServerImpl` implementation
use super::ServerImpl;

use drv_spi_api::SpiServer;

use host_sp_messages::{InventoryData, InventoryDataResult};

userlib::task_slot!(SPI, spi_driver);

// the CPU interrupt is not connected on grapefruit, so pick an
// unconnected GPIO
pub(crate) const SP_TO_HOST_CPU_INT_L: drv_stm32xx_sys_api::PinSet =
    drv_stm32xx_sys_api::Port::B.pin(1);
pub(crate) const SP_TO_HOST_CPU_INT_TYPE: drv_stm32xx_sys_api::OutputType =
    drv_stm32xx_sys_api::OutputType::OpenDrain;

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 2;

    /// Look up a device in our inventory, by index
    ///
    /// Indexes are assigned arbitrarily and may change freely with SP
    /// revisions.
    ///
    /// On success, we will have already filled `self.tx_buf` with our response;
    /// this _may_ be an error if the index was valid but we can't communicate
    /// with the target device.
    ///
    /// This function should only return an error if the index is invalid;
    /// in that case, our caller is responsible for encoding the error as
    /// ```
    /// SpToHost::InventoryData{
    ///     result: err
    ///     name: [0; u32],
    /// }
    /// ```
    pub(crate) fn perform_inventory_lookup(
        &mut self,
        sequence: u64,
        index: u32,
    ) -> Result<(), InventoryDataResult> {
        #[forbid(unreachable_patterns)]
        match index {
            // Grapefruit has a FRU ID EEPROM, but it's connected to the FPGA,
            // so it's obnoxious to read here
            0 => {
                // U12: the service processor itself
                // The UID is readable by stm32xx_sys
                let sys =
                    drv_stm32xx_sys_api::Sys::from(crate::SYS.get_task_id());
                let uid = sys.read_uid();

                let idc = drv_stm32h7_dbgmcu::read_idc();
                let dbgmcu_rev_id = (idc >> 16) as u16;
                let dbgmcu_dev_id = (idc & 4095) as u16;
                *self.scratch = InventoryData::Stm32H7 {
                    uid,
                    dbgmcu_rev_id,
                    dbgmcu_dev_id,
                };
                self.tx_buf.try_encode_inventory(sequence, b"U12", || {
                    Ok(self.scratch)
                });
            }

            1 => {
                let spi = drv_spi_api::Spi::from(SPI.get_task_id());
                let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
                let ksz8463 = ksz8463::Ksz8463::new(ksz8463_dev);
                *self.scratch = InventoryData::Ksz8463 { cider: 0 };
                self.tx_buf.try_encode_inventory(sequence, b"U401", || {
                    let InventoryData::Ksz8463 { cider } = self.scratch else {
                        unreachable!();
                    };
                    *cider = ksz8463
                        .read(ksz8463::Register::CIDER)
                        .map_err(|_| InventoryDataResult::DeviceFailed)?;
                    Ok(self.scratch)
                });
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
