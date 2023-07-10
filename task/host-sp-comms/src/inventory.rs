// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SP inventory types and implementation
//!
//! This reduces clutter in the main `ServerImpl` implementation
use super::ServerImpl;
use drv_i2c_api::I2cDevice;
use userlib::task_slot;

use host_sp_messages::{InventoryData, InventoryDataResult};

/// Number of devices in our inventory
pub(crate) const INVENTORY_COUNT: u32 = 18;

/// Inventory API version (always 0 for now)
pub(crate) const INVENTORY_API_VERSION: u32 = 0;

// We need to query some devices over I2C
task_slot!(I2C, i2c_driver);

impl ServerImpl {
    /// On success, we will have already filled `self.tx_buf` with our response.
    /// On failure, our caller should response with
    /// `SpToHost::KeyLookupResult(err)` with the error we return.
    pub(crate) fn perform_inventory_lookup(
        &mut self,
        sequence: u64,
        index: u32,
    ) -> Result<(), InventoryDataResult> {
        #[forbid(unreachable_patterns)]
        match index {
            i @ 0..=15 => {
                self.dimm_inventory_lookup(sequence, i);
            }
            16 => {
                // U615/ID: SP barcode is available in packrat
                let packrat = &self.packrat;
                self.tx_buf
                    .try_encode_inventory(sequence, b"U615/ID", |buf| {
                        let id = packrat
                            .get_identity()
                            .map_err(|_| InventoryDataResult::DeviceAbsent)?;
                        let d = InventoryData::VpdIdentity(id);
                        let n = hubpack::serialize(buf, &d)?;
                        Ok(n)
                    });
            }
            17 => {
                // U615: Gimlet VPD EEPROM
                self.read_at24csw080_id(
                    sequence,
                    b"U615",
                    i2c_config::devices::at24csw080_local_vpd,
                )
            }
            // We need to specify INVENTORY_COUNT individually here to trigger
            // an error if we've overlapped it with a previous range
            INVENTORY_COUNT | INVENTORY_COUNT..=u32::MAX => {
                return Err(InventoryDataResult::InvalidIndex)
            }
        }

        Ok(())
    }

    fn dimm_inventory_lookup(&mut self, sequence: u64, index: u32) {
        // Build a name of the form `m{index}`, to match the designator
        let mut name = [0; 32];
        name[0] = b'm';
        if index >= 10 {
            name[1] = b'0' + (index / 10) as u8;
            name[2] = b'0' + (index % 10) as u8;
        } else {
            name[1] = b'0' + index as u8;
        }

        let packrat = &self.packrat; // partial borrow
        self.tx_buf.try_encode_inventory(sequence, &name, |buf| {
            // TODO: does packrat index match PCA designator?
            if packrat.get_spd_present(index as usize) {
                let mut out = [0u8; 512];
                packrat.get_full_spd_data(index as usize, out.as_mut_slice());
                let n = hubpack::serialize(buf, &InventoryData::DimmSpd(out))
                    .map_err(|_| InventoryDataResult::SerializationError)?;
                Ok(n)
            } else {
                Err(InventoryDataResult::DeviceAbsent)
            }
        });
    }

    fn read_at24csw080_id(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
    ) {
        use drv_i2c_api::ResponseCode;
        use drv_i2c_devices::at24csw080::{At24Csw080, Error};

        let dev = At24Csw080::new(f(I2C.get_task_id()));
        self.tx_buf.try_encode_inventory(sequence, name, |buf| {
            let mut id = [0u8; 16];
            for (i, b) in id.iter_mut().enumerate() {
                // TODO: make this a single IPC call?
                *b = dev.read_security_register_byte(i as u8).map_err(|e| {
                    match e {
                        Error::I2cError(ResponseCode::NoDevice) => {
                            InventoryDataResult::DeviceAbsent
                        }
                        _ => InventoryDataResult::DeviceFailed,
                    }
                })?;
            }
            let d = InventoryData::At24csw08xSerial(id.into());
            let n = hubpack::serialize(buf, &d)?;
            Ok(n)
        });
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
