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
use drv_spi_api::SpiServer;
use userlib::TaskId;
use zerocopy::AsBytes;

use host_sp_messages::{InventoryData, InventoryDataResult};

userlib::task_slot!(I2C, i2c_driver);
userlib::task_slot!(SPI, spi_driver);

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 60;

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
                let mut data = InventoryData::VpdIdentity(Default::default());
                self.tx_buf.try_encode_inventory(sequence, b"U615/ID", || {
                    let InventoryData::VpdIdentity(identity) = &mut data
                        else { unreachable!(); };
                    *identity = packrat
                        .get_identity()
                        .map_err(|_| InventoryDataResult::DeviceAbsent)?
                        .into();
                    Ok(&data)
                });
            }
            17 => {
                // U615: Gimlet VPD EEPROM
                //
                // Note that for VPD AT24CSW080 identities, we allocate our
                // InventoryData in the outer frame then pass it in as a
                // reference; `read_at24csw080_id` typically isn't inlined, and
                // we're already paying a stack frame for the data in this
                // function, so it saves us 512 bytes of stack.
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(
                    sequence,
                    b"U615",
                    i2c_config::devices::at24csw080_local_vpd,
                    &mut data,
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
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(
                    sequence,
                    b"J180",
                    i2c_config::devices::at24csw080_fan_vpd,
                    &mut data,
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
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(sequence, &name, f, &mut data)
            }
            // TODO: Sharkfin HSC?
            40 => {
                // U12: the service processor itself
                // The UID is readable by stm32xx_sys
                let sys =
                    drv_stm32xx_sys_api::Sys::from(crate::SYS.get_task_id());
                let uid = sys.read_uid();

                let idc = drv_stm32h7_dbgmcu::read_idc();
                let dbgmcu_rev_id = (idc >> 16) as u16;
                let dbgmcu_dev_id = (idc & 4095) as u16;
                let data = InventoryData::Stm32H7 {
                    uid,
                    dbgmcu_rev_id,
                    dbgmcu_dev_id,
                };
                self.tx_buf
                    .try_encode_inventory(sequence, b"U12", || Ok(&data));
            }
            41 => {
                // U431: BRM491
                let dev = i2c_config::devices::bmr491_ibc(I2C.get_task_id());
                let name = b"U431";
                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                let mut data = InventoryData::Bmr491 {
                    mfr_id: [0u8; 12],
                    mfr_model: [0u8; 20],
                    mfr_revision: [0u8; 12],
                    mfr_location: [0u8; 12],
                    mfr_date: [0u8; 12],
                    mfr_serial: [0u8; 20],
                    mfr_firmware_data: [0u8; 20],
                };
                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::bmr491::CommandCode;
                        let InventoryData::Bmr491 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_location,
                            mfr_date,
                            mfr_serial,
                            mfr_firmware_data,
                        } = &mut data else { unreachable!() };
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
                        Ok(&data)
                    },
                )
            }

            42 => {
                // U432: ISL68224
                let dev = i2c_config::devices::isl68224(I2C.get_task_id())[0];
                let name = b"U432";
                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                let mut data = InventoryData::Isl68224 {
                    mfr_id: [0u8; 4],
                    mfr_model: [0u8; 4],
                    mfr_revision: [0u8; 4],
                    mfr_date: [0u8; 4],
                    ic_device_id: [0u8; 4],
                    ic_device_rev: [0u8; 4],
                };
                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::isl68224::CommandCode;
                        let InventoryData::Isl68224 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_date,
                            ic_device_id,
                            ic_device_rev,
                        } = &mut data else { unreachable!() };
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
                        Ok(&data)
                    },
                )
            }
            43 | 44 => {
                let dev = i2c_config::devices::raa229618(I2C.get_task_id())
                    [(index - 43) as usize];
                let mut name = *b"U350";
                name[3] += (index - 43) as u8;

                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                let mut data = InventoryData::Raa229618 {
                    mfr_id: [0u8; 4],
                    mfr_model: [0u8; 4],
                    mfr_revision: [0u8; 4],
                    mfr_date: [0u8; 4],
                    ic_device_id: [0u8; 4],
                    ic_device_rev: [0u8; 4],
                };
                self.tx_buf.try_encode_inventory(
                    sequence,
                    name.as_slice(),
                    || {
                        use pmbus::commands::raa229618::CommandCode;
                        let InventoryData::Raa229618 {
                            mfr_id,
                            mfr_model,
                            mfr_revision,
                            mfr_date,
                            ic_device_id,
                            ic_device_rev,
                        } = &mut data else { unreachable!() };
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
                        Ok(&data)
                    },
                )
            }

            45..=49 => {
                const TABLE: [(&[u8], fn(TaskId) -> I2cDevice); 5] = [
                    (b"U522", i2c_config::devices::tps546b24a_v3p3_sp_a2),
                    (b"U560", i2c_config::devices::tps546b24a_v3p3_sys_a0),
                    (b"U524", i2c_config::devices::tps546b24a_v5p0_sys_a2),
                    (b"U561", i2c_config::devices::tps546b24a_v1p8_sys_a2),
                    (b"U565", i2c_config::devices::tps546b24a_v0p96_nic),
                ];
                let (name, f) = TABLE[(index - 45) as usize];

                let dev = f(I2C.get_task_id());
                let mut data = InventoryData::Tps546b24a {
                    mfr_id: [0u8; 3],
                    mfr_model: [0u8; 3],
                    mfr_revision: [0u8; 3],
                    mfr_serial: [0u8; 3],
                    ic_device_id: [0u8; 6],
                    ic_device_rev: [0u8; 2],
                    nvm_checksum: 0u16,
                };
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let InventoryData::Tps546b24a {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_serial,
                        ic_device_id,
                        ic_device_rev,
                        nvm_checksum,
                    } = &mut data else { unreachable!() };
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
                    dev.read_reg_into(
                        CommandCode::NVM_CHECKSUM as u8,
                        nvm_checksum.as_bytes_mut(),
                    )?;
                    Ok(&data)
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

                let mut data = InventoryData::Adm1272 {
                    mfr_id: [0u8; 3],
                    mfr_model: [0u8; 10],
                    mfr_revision: [0u8; 2],
                    mfr_date: [0u8; 6],
                };
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let InventoryData::Adm1272 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_date,
                    } = &mut data else { unreachable!() };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                    Ok(&data)
                })
            }

            52..=57 => {
                let i = index as usize - 52;
                // XXX this assumes that designator order is matched in the TOML
                // file and in our list of names below!
                let dev = i2c_config::devices::tmp117(I2C.get_task_id())[i];
                let name = match i {
                    0 => b"J194/U1",
                    1 => b"J195/U1",
                    2 => b"J196/U1",
                    3 => b"J197/U1",
                    4 => b"J198/U1",
                    5 => b"J199/U1",
                    _ => unreachable!(),
                };
                let mut data = InventoryData::Tmp117 {
                    id: 0,
                    eeprom1: 0,
                    eeprom2: 0,
                    eeprom3: 0,
                };
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    let InventoryData::Tmp117 {
                        id,
                        eeprom1,
                        eeprom2,
                        eeprom3 } = &mut data else { unreachable!(); };
                    *id = dev.read_reg(0x0Fu8)?;
                    *eeprom1 = dev.read_reg(0x05u8)?;
                    *eeprom2 = dev.read_reg(0x06u8)?;
                    *eeprom3 = dev.read_reg(0x08u8)?;
                    Ok(&data)
                })
            }

            58 => {
                let dev = i2c_config::devices::idt8a34003(I2C.get_task_id())[0];
                let name = b"U446";
                let mut data = InventoryData::Idt8a34003 {
                    hw_rev: 0,
                    major_rel: 0,
                    minor_rel: 0,
                    hotfix_rel: 0,
                    product_id: 0,
                };
                self.tx_buf.try_encode_inventory(sequence, name, || {
                    let InventoryData::Idt8a34003 {
                        hw_rev,
                        major_rel,
                        minor_rel,
                        hotfix_rel,
                        product_id,
                    } = &mut data else { unreachable!(); };
                    // This chip includes a separate register that controls the
                    // upper address byte, i.e. a paged memory implementation.
                    // We'll use `write_read_reg` to avoid the possibility of
                    // race conditions here.
                    *hw_rev = dev.write_read_reg(
                        0x1eu8,
                        &[0xfc, 0x00, 0xc0, 0x10, 0x20],
                    )?;
                    *major_rel = dev.write_read_reg(
                        0x24u8,
                        &[0xfc, 0x00, 0xc0, 0x10, 0x20],
                    )?;
                    *minor_rel = dev.write_read_reg(
                        0x25u8,
                        &[0xfc, 0x00, 0xc0, 0x10, 0x20],
                    )?;
                    *hotfix_rel = dev.write_read_reg(
                        0x26u8,
                        &[0xfc, 0x00, 0xc0, 0x10, 0x20],
                    )?;
                    *product_id = dev.write_read_reg(
                        0x32u8,
                        &[0xfc, 0x00, 0xc0, 0x10, 0x20],
                    )?;
                    Ok(&data)
                })
            }

            59 => {
                let spi = drv_spi_api::Spi::from(SPI.get_task_id());
                let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
                let ksz8463 = ksz8463::Ksz8463::new(ksz8463_dev);
                let mut data = InventoryData::Ksz8463 { cider: 0 };
                self.tx_buf.try_encode_inventory(sequence, b"U401", || {
                    let InventoryData::Ksz8463 { cider } = &mut data
                            else { unreachable!(); };
                    *cider = ksz8463
                        .read(ksz8463::Register::CIDER)
                        .map_err(|_| InventoryDataResult::DeviceFailed)?;
                    Ok(&data)
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
        name[0] = b'M';
        if index >= 10 {
            name[1] = b'0' + (index / 10) as u8;
            name[2] = b'0' + (index % 10) as u8;
        } else {
            name[1] = b'0' + index as u8;
        }

        let packrat = &self.packrat; // partial borrow
        let mut data = InventoryData::DimmSpd([0u8; 512]);
        self.tx_buf.try_encode_inventory(sequence, &name, || {
            // TODO: does packrat index match PCA designator?
            if packrat.get_spd_present(index as usize) {
                let InventoryData::DimmSpd(out) = &mut data
                    else { unreachable!(); };
                packrat.get_full_spd_data(index as usize, out);
                Ok(&data)
            } else {
                Err(InventoryDataResult::DeviceAbsent)
            }
        });
    }

    /// Reads the 128-byte unique ID from an AT24CSW080 EEPROM
    ///
    /// `data` is passed in to reduce stack frame size, since we already require
    /// an allocation for it on the caller's stack frame.
    fn read_at24csw080_id(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
        data: &mut InventoryData,
    ) {
        // This should be done by the caller, but let's make it obviously
        // correct (since we destructure it below).
        *data = InventoryData::At24csw08xSerial([0u8; 16]);
        let dev = At24Csw080::new(f(I2C.get_task_id()));
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::At24csw08xSerial(id) = data
                else { unreachable!(); };
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
            Ok(&data)
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
        let mut data = InventoryData::VpdIdentity(Default::default());
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::VpdIdentity(identity) = &mut data
                else { unreachable!(); };
            *identity = read_one_barcode(dev, &[(*b"BARC", 0)])?.into();
            Ok(&data)
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
        let mut data = InventoryData::FanIdentity {
            identity: Default::default(),
            vpd_identity: Default::default(),
            fans: Default::default(),
        };
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::FanIdentity {
                identity,
                vpd_identity,
                fans: [fan0, fan1, fan2]
            } = &mut data else { unreachable!(); };
            *identity = read_one_barcode(dev.clone(), &[(*b"BARC", 0)])?.into();
            *vpd_identity =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 0)])?
                    .into();
            *fan0 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 1)])?
                    .into();
            *fan1 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 2)])?
                    .into();
            *fan2 =
                read_one_barcode(dev.clone(), &[(*b"SASY", 0), (*b"BARC", 3)])?
                    .into();
            Ok(&data)
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
