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
use drv_oxide_vpd::VpdError;
use userlib::TaskId;
use zerocopy::AsBytes;

use host_sp_messages::{InventoryData, InventoryDataResult};

userlib::task_slot!(I2C, i2c_driver);

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 58;

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
            0..=15 => {
                self.dimm_inventory_lookup(sequence, index);
            }
            16 => {
                // U615/ID: SP barcode is available in packrat
                let packrat = &self.packrat;
                self.tx_buf.try_encode_inventory(sequence, b"U615/ID", || {
                    let id = packrat
                        .get_identity()
                        .map_err(|_| InventoryDataResult::DeviceAbsent)?;
                    let d = InventoryData::VpdIdentity(id);
                    Ok(d)
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
                self.read_fan_barcodes(
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
            20..=29 => {
                let (designator, f) =
                    Self::get_sharkfin_vpd(index as usize - 20);
                let mut name = *b"____/U7/ID";
                name[0..4].copy_from_slice(&designator);
                self.read_eeprom_barcode(sequence, &name, f)
            }
            30..=39 => {
                let (designator, f) =
                    Self::get_sharkfin_vpd(index as usize - 30);
                let mut name = *b"____/U7";
                name[0..4].copy_from_slice(&designator);
                self.read_at24csw080_id(sequence, &name, f)
            }
            // TODO: Sharkfin HSC?
            40 => {
                // U12: the service processor itself
                // The UID is readable by stm32xx_sys
                let sys =
                    drv_stm32xx_sys_api::Sys::from(crate::SYS.get_task_id());
                let uid = sys.read_uid();

                self.tx_buf.try_encode_inventory(sequence, b"U12", || {
                    Ok(InventoryData::Stm32H7 {
                        uid,
                        dbgmcu_rev_id: 0, // TODO
                        dbgmcu_dev_id: 0, // TODO
                    })
                });
            }
            41 => {
                // U431: BRM491
                let dev = i2c_config::devices::bmr491_ibc(I2C.get_task_id());
                let name = b"U431";
                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::bmr491::CommandCode;
                        // To be stack-friendly, we declare our output here,
                        // then bind references to all the member variables.
                        let mut out = InventoryData::Bmr491 {
                            mfr_id: [0u8; 12],
                            mfr_model: [0u8; 20],
                            mfr_revision: [0u8; 12],
                            mfr_location: [0u8; 12],
                            mfr_date: [0u8; 12],
                            mfr_serial: [0u8; 20],
                            mfr_firmware_data: [0u8; 20],
                        };
                        let InventoryData::Bmr491 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_location,
                            mfr_date,
                            mfr_serial,
                            mfr_firmware_data,
                        } = &mut out else { unreachable!() };
                        dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                        dev.read_block(
                            CommandCode::MFR_MODEL as u8,
                            mfr_model,
                        )?;
                        dev.read_block(
                            CommandCode::MFR_REVISION as u8,
                            mfr_revision,
                        )?;
                        dev.read_block(
                            CommandCode::MFR_LOCATION as u8,
                            mfr_location,
                        )?;
                        dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                        dev.read_block(
                            CommandCode::MFR_SERIAL as u8,
                            mfr_serial,
                        )?;
                        dev.read_block(
                            CommandCode::MFR_FIRMWARE_DATA as u8,
                            mfr_firmware_data,
                        )?;
                        Ok(out)
                    },
                )
            }

            42 => {
                // U432: ISL68224
                let dev = i2c_config::devices::isl68224(I2C.get_task_id())[0];
                let name = b"U432";
                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::isl68224::CommandCode;
                        // To be stack-friendly, we declare our output here,
                        // then bind references to all the member variables.
                        let mut out = InventoryData::Isl68224 {
                            mfr_id: [0u8; 4],
                            mfr_model: [0u8; 4],
                            mfr_revision: [0u8; 4],
                            mfr_date: [0u8; 4],
                            ic_device_id: [0u8; 4],
                            ic_device_rev: [0u8; 4],
                        };
                        let InventoryData::Isl68224 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_date,
                            ic_device_id,
                            ic_device_rev,
                        } = &mut out else { unreachable!() };
                        dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                        dev.read_block(
                            CommandCode::MFR_MODEL as u8,
                            mfr_model,
                        )?;
                        dev.read_block(
                            CommandCode::MFR_REVISION as u8,
                            mfr_revision,
                        )?;
                        dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                        dev.read_block(
                            CommandCode::IC_DEVICE_ID as u8,
                            ic_device_id,
                        )?;
                        dev.read_block(
                            CommandCode::IC_DEVICE_REV as u8,
                            ic_device_rev,
                        )?;
                        Ok(out)
                    },
                )
            }
            43 | 44 => {
                let dev = i2c_config::devices::raa229618(I2C.get_task_id())
                    [(index - 43) as usize];
                let mut name = *b"U350";
                name[3] += (index - 43) as u8;

                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::raa229618::CommandCode;
                        // To be stack-friendly, we declare our output here,
                        // then bind references to all the member variables.
                        let mut out = InventoryData::Raa229618 {
                            mfr_id: [0u8; 4],
                            mfr_model: [0u8; 4],
                            mfr_revision: [0u8; 4],
                            mfr_date: [0u8; 4],
                            ic_device_id: [0u8; 4],
                            ic_device_rev: [0u8; 4],
                        };
                        let InventoryData::Raa229618 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_date,
                            ic_device_id,
                            ic_device_rev,
                        } = &mut out else { unreachable!() };
                        dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                        dev.read_block(
                            CommandCode::MFR_MODEL as u8,
                            mfr_model,
                        )?;
                        dev.read_block(
                            CommandCode::MFR_REVISION as u8,
                            mfr_revision,
                        )?;
                        dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                        dev.read_block(
                            CommandCode::IC_DEVICE_ID as u8,
                            ic_device_id,
                        )?;
                        dev.read_block(
                            CommandCode::IC_DEVICE_REV as u8,
                            ic_device_rev,
                        )?;
                        Ok(out)
                    },
                )
            }

            45..=49 => {
                const TABLE: [(&[u8], fn(TaskId) -> I2cDevice); 5] = [
                    (b"u522", i2c_config::devices::tps546b24a_v3p3_sp_a2),
                    (b"u560", i2c_config::devices::tps546b24a_v3p3_sys_a0),
                    (b"u524", i2c_config::devices::tps546b24a_v5p0_sys_a2),
                    (b"u561", i2c_config::devices::tps546b24a_v1p8_sys_a2),
                    (b"u565", i2c_config::devices::tps546b24a_v0p96_nic),
                ];
                let (name, f) = TABLE[(index - 45) as usize];

                let dev = f(I2C.get_task_id());
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let mut out = InventoryData::Tps546b24a {
                        mfr_id: [0u8; 3],
                        mfr_model: [0u8; 3],
                        mfr_revision: [0u8; 3],
                        mfr_serial: [0u8; 3],
                        ic_device_id: [0u8; 6],
                        ic_device_rev: [0u8; 2],
                        nvm_checksum: 0u16,
                    };
                    let InventoryData::Tps546b24a {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_serial,
                        ic_device_id,
                        ic_device_rev,
                        nvm_checksum,
                    } = &mut out else { unreachable!() };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    dev.read_block(CommandCode::MFR_SERIAL as u8, mfr_serial)?;
                    dev.read_block(
                        CommandCode::IC_DEVICE_ID as u8,
                        ic_device_id,
                    )?;
                    dev.read_block(
                        CommandCode::IC_DEVICE_REV as u8,
                        ic_device_rev,
                    )?;
                    dev.read_block(
                        CommandCode::NVM_CHECKSUM as u8,
                        nvm_checksum.as_bytes_mut(),
                    )?;
                    Ok(out)
                })
            }
            50 | 51 => {
                let dev = i2c_config::devices::adm1272(I2C.get_task_id())
                    [(index - 50) as usize];
                // U452 and U419, both ADM1272
                let name = match index {
                    50 => b"U419",
                    51 => b"U452",
                    _ => unreachable!(),
                };

                self.tx_buf.try_encode_inventory(sequence, name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let mut out = InventoryData::Adm1272 {
                        mfr_id: [0u8; 3],
                        mfr_model: [0u8; 10],
                        mfr_revision: [0u8; 2],
                        mfr_date: [0u8; 6],
                    };
                    let InventoryData::Adm1272 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_date,
                    } = &mut out else { unreachable!() };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                    Ok(out)
                })
            }

            52..=57 => {
                let i = index as usize - 52;
                // XXX this assumes that designator order is matched in the TOML
                // file and in our list of names below!
                let dev = i2c_config::devices::tmp117(I2C.get_task_id())[i];
                let name = match i {
                    0 => b"J194",
                    1 => b"J195",
                    2 => b"J196",
                    3 => b"J197",
                    4 => b"J198",
                    5 => b"J199",
                    _ => unreachable!(),
                };
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    let id: u16 = dev.read_reg(0x0F)?;
                    let eeprom1: u16 = dev.read_reg(0x05)?;
                    let eeprom2: u16 = dev.read_reg(0x06)?;
                    let eeprom3: u16 = dev.read_reg(0x08)?;
                    Ok(InventoryData::Tmp117 {
                        id,
                        eeprom1,
                        eeprom2,
                        eeprom3,
                    })
                })
            }

            // We need to specify INVENTORY_COUNT individually here to trigger
            // an error if we've overlapped it with a previous range
            Self::INVENTORY_COUNT | Self::INVENTORY_COUNT..=u32::MAX => {
                return Err(InventoryDataResult::InvalidIndex)
            }
        }

        Ok(())
    }

    /// Looks up a Sharkfin VPD EEPROM by sharkfin index (0-9)
    ///
    /// Returns a designator (e.g. J206) and constructor function
    fn get_sharkfin_vpd(i: usize) -> ([u8; 4], fn(TaskId) -> I2cDevice) {
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
            i2c_config::devices::at24csw080_sharkfin_j_vpd,
        ];
        // The base name is J206, so we count up from there
        let mut name = *b"J2__";
        name[2] = ((i + 6) / 10) as u8 + b'0';
        name[3] = ((i + 6) % 10) as u8 + b'0';
        (name, fs[i])
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
        self.tx_buf.try_encode_inventory(sequence, &name, || {
            // TODO: does packrat index match PCA designator?
            if packrat.get_spd_present(index as usize) {
                let mut out = [0u8; 512];
                packrat.get_full_spd_data(index as usize, out.as_mut_slice());
                Ok(InventoryData::DimmSpd(out))
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
        self.tx_buf.try_encode_inventory(sequence, name, || {
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
            Ok(InventoryData::At24csw08xSerial(id))
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
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let identity = read_one_barcode(dev, &[(*b"BARC", 0)])?;
            Ok(InventoryData::VpdIdentity(identity))
        })
    }

    /// Reads the fan EEPROM barcode values
    ///
    /// The fan EEPROM includes nested barcodes:
    /// - The top-level `BARC`, for the assembly
    /// - A nested value `SASY`, which contains four more `BARC` values for each
    ///   individual fan
    ///
    /// On success, packs the barcode into `self.tx_buf`; on failure, return an
    /// error (`DeviceAbsent` if we saw `NoDevice`, or `DeviceFailed` on all
    /// other errors).
    fn read_fan_barcodes(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
    ) {
        let dev = f(I2C.get_task_id());
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let identity = read_one_barcode(dev.clone(), &[(*b"BARC", 0)])?;
            let barc0 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 0)])?;
            let barc1 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 1)])?;
            let barc2 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 2)])?;
            let barc3 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 3)])?;
            Ok(InventoryData::FanIdentity {
                identity,
                fans: [barc0, barc1, barc2, barc3],
            })
        })
    }
}

/// Free function to read a nested barcode, translating errors appropriately
fn read_one_barcode(
    dev: I2cDevice,
    path: &[([u8; 4], usize)],
) -> Result<oxide_barcode::VpdIdentity, InventoryDataResult> {
    let eeprom = At24Csw080::new(dev.clone());
    let mut barcode = [0; 32];
    match drv_oxide_vpd::read_config_nested_from_into(
        eeprom,
        path,
        &mut barcode,
    ) {
        Ok(n) => {
            // extract barcode!
            let identity = oxide_barcode::VpdIdentity::parse(&barcode[..n])
                .map_err(|_| InventoryDataResult::DeviceFailed)?;
            Ok(identity)
        }
        Err(
            VpdError::ErrorOnBegin(err)
            | VpdError::ErrorOnRead(err)
            | VpdError::ErrorOnNext(err)
            | VpdError::InvalidChecksum(err),
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
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
