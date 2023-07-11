// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SP inventory types and implementation
//!
//! This reduces clutter in the main `ServerImpl` implementation
use super::ServerImpl;
use drv_i2c_api::I2cDevice;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::at24csw080::{At24Csw080, Error as EepromError};
use drv_local_vpd::LocalVpdError;
use userlib::task_slot;

use host_sp_messages::{InventoryData, InventoryDataResult};

/// Number of devices in our inventory
pub(crate) const INVENTORY_COUNT: u32 = 40;

/// Inventory API version (always 0 for now)
pub(crate) const INVENTORY_API_VERSION: u32 = 0;

// We need to query some devices over I2C
task_slot!(I2C, i2c_driver);

impl ServerImpl {
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
            18 => {
                // J180/ID: Fan VPD barcode (not available in packrat)
                self.read_eeprom_barcode(
                    sequence,
                    b"J180/ID",
                    i2c_config::devices::at24csw080_fan_vpd,
                )
            }
            19 => {
                // J180: Fan VPD EEPROM
                self.read_at24csw080_id(
                    sequence,
                    b"J180",
                    i2c_config::devices::at24csw080_fan_vpd,
                )
            }
            // Welcome to The Sharkfin Zone
            //
            // Each Sharkfin has 3 inventory items:
            // - Oxide barcode
            // - Raw VPD EEPROM ID register
            // - Hot-swap controller
            //
            // Sharkfin connectors start at J206 and are numbered sequentially
            i @ (20..=29) => {
                let i = i - 20;
                let fs = [
                    i2c_config::devices::at24csw080_sharkfin_a_vpd,
                    i2c_config::devices::at24csw080_sharkfin_b_vpd,
                    i2c_config::devices::at24csw080_sharkfin_c_vpd,
                    i2c_config::devices::at24csw080_sharkfin_d_vpd,
                    i2c_config::devices::at24csw080_sharkfin_e_vpd,
                    i2c_config::devices::at24csw080_sharkfin_f_vpd,
                    i2c_config::devices::at24csw080_sharkfin_g_vpd,
                    i2c_config::devices::at24csw080_sharkfin_h_vpd,
                    i2c_config::devices::at24csw080_sharkfin_i_vpd,
                ];
                let mut name = *b"J200/U7/ID";
                let designator = 6 + i; // Starts at J206
                name[2] += (designator / 10) as u8;
                name[3] += (designator % 10) as u8;
                self.read_eeprom_barcode(sequence, &name, fs[i as usize])
            }
            i @ (30..=39) => {
                let i = i - 30;
                let fs = [
                    i2c_config::devices::at24csw080_sharkfin_a_vpd,
                    i2c_config::devices::at24csw080_sharkfin_b_vpd,
                    i2c_config::devices::at24csw080_sharkfin_c_vpd,
                    i2c_config::devices::at24csw080_sharkfin_d_vpd,
                    i2c_config::devices::at24csw080_sharkfin_e_vpd,
                    i2c_config::devices::at24csw080_sharkfin_f_vpd,
                    i2c_config::devices::at24csw080_sharkfin_g_vpd,
                    i2c_config::devices::at24csw080_sharkfin_h_vpd,
                    i2c_config::devices::at24csw080_sharkfin_i_vpd,
                ];
                let mut name = *b"J200/U7";
                let designator = 6 + i; // Starts at J206
                name[2] += (designator / 10) as u8;
                name[3] += (designator % 10) as u8;
                self.read_at24csw080_id(sequence, &name, fs[i as usize])
            }
            /*
            21 => self.read_at24csw080_id(
                sequence,
                b"J206/U7",
                i2c_config::devices::at24csw080_sharkfin_a_vpd,
            ),
            */
            // TODO: Sharkfin HSC?

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

    /// Reads the 128-byte unique ID from an AT24CSW080 EEPROM
    fn read_at24csw080_id(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
    ) {
        let dev = At24Csw080::new(f(I2C.get_task_id()));
        self.tx_buf.try_encode_inventory(sequence, name, |buf| {
            let mut id = [0u8; 16];
            for (i, b) in id.iter_mut().enumerate() {
                // TODO: add an API to make this a single IPC call?
                *b = dev.read_security_register_byte(i as u8).map_err(|e| {
                    match e {
                        EepromError::I2cError(ResponseCode::NoDevice) => {
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

    /// Reads the "BARC" value from a TLV-C blob in an AT24CSW080 EEPROM
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    fn read_eeprom_barcode(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
    ) {
        let dev = f(I2C.get_task_id());
        let eeprom = At24Csw080::new(dev);
        self.tx_buf.try_encode_inventory(sequence, name, |buf| {
            let mut barcode = [0; 32];
            match drv_local_vpd::read_config_from_into(
                eeprom,
                *b"BARC",
                &mut barcode,
            ) {
                Ok(n) => {
                    // extract barcode!
                    let identity =
                        oxide_barcode::VpdIdentity::parse(&barcode[..n])
                            .map_err(|_| InventoryDataResult::DeviceFailed)?;
                    let d = InventoryData::VpdIdentity(identity);
                    let n = hubpack::serialize(buf, &d)?;
                    Ok(n)
                }
                Err(
                    LocalVpdError::ErrorOnBegin(err)
                    | LocalVpdError::ErrorOnRead(err)
                    | LocalVpdError::ErrorOnNext(err)
                    | LocalVpdError::InvalidChecksum(err),
                ) if err
                    == tlvc::TlvcReadError::User(EepromError::I2cError(
                        ResponseCode::NoDevice,
                    )) =>
                {
                    // TODO: ringbuf logging here?
                    Err(InventoryDataResult::DeviceAbsent)
                }
                Err(..) => Err(InventoryDataResult::DeviceFailed),
            }
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
