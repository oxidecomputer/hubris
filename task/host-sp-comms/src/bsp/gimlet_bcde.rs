// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SP inventory types and implementation
//!
//! This reduces clutter in the main `ServerImpl` implementation
use super::{inventory::by_refdes, ServerImpl};

use drv_i2c_api::I2cDevice;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::at24csw080::{At24Csw080, Error as EepromError};
use drv_oxide_vpd::VpdError;
use drv_spi_api::SpiServer;
use task_sensor_api::SensorId;
use userlib::TaskId;
use zerocopy::IntoBytes;

use host_sp_messages::{InventoryData, InventoryDataResult};

userlib::task_slot!(I2C, i2c_driver);
userlib::task_slot!(SPI, spi_driver);

// This net is named SP_TO_SP3_INT_L in the schematic
pub(crate) const SP_TO_HOST_CPU_INT_L: drv_stm32xx_sys_api::PinSet =
    drv_stm32xx_sys_api::Port::I.pin(7);
pub(crate) const SP_TO_HOST_CPU_INT_TYPE: drv_stm32xx_sys_api::OutputType =
    drv_stm32xx_sys_api::OutputType::OpenDrain;

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 72;

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
                self.dimm_inventory_lookup(sequence, index as u8);
            }
            16 => {
                // U615/ID: SP barcode is available in packrat
                let packrat = &self.packrat;
                *self.scratch = InventoryData::VpdIdentity(Default::default());
                self.tx_buf.try_encode_inventory(sequence, b"U615/ID", || {
                    let InventoryData::VpdIdentity(identity) = self.scratch
                    else {
                        unreachable!();
                    };
                    *identity = packrat
                        .get_identity()
                        .map_err(|_| InventoryDataResult::DeviceAbsent)?
                        .into();
                    Ok(self.scratch)
                });
            }
            17 => {
                // U615: Gimlet VPD EEPROM
                let (name, f, _sensors) = by_refdes!(U615, at24csw080);
                self.read_at24csw080_id(sequence, &name, f)
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
                // J180: Fan VPD EEPROM (on the daughterboard)
                self.read_at24csw080_id(
                    sequence,
                    b"J180/U1",
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
                let (designator, f): ([u8; 7], _) =
                    Self::get_sharkfin_vpd(index as usize - 20);
                let mut name = *b"____/U7/ID";
                name[0..4].copy_from_slice(&designator);
                self.read_eeprom_barcode(sequence, &name, f)
            }
            30..=39 => {
                let (mut designator, f): ([u8; 7], _) =
                    Self::get_sharkfin_vpd(index as usize - 30);
                designator[4] = b'/';
                self.read_at24csw080_id(sequence, &designator, f)
            }
            40 => {
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
            41 => {
                // U431: BMR491
                let (name, f, sensors) = by_refdes!(U431, bmr491);
                let dev = f(I2C.get_task_id());
                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                *self.scratch = InventoryData::Bmr491 {
                    mfr_id: [0u8; 12],
                    mfr_model: [0u8; 20],
                    mfr_revision: [0u8; 12],
                    mfr_location: [0u8; 12],
                    mfr_date: [0u8; 12],
                    mfr_serial: [0u8; 20],
                    mfr_firmware_data: [0u8; 20],
                    temp_sensor: sensors.temperature.into(),
                    voltage_sensor: sensors.voltage.into(),
                    current_sensor: sensors.current.into(),
                    power_sensor: sensors.power.into(),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::bmr491::CommandCode;
                    let InventoryData::Bmr491 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_location,
                        mfr_date,
                        mfr_serial,
                        mfr_firmware_data,
                        temp_sensor: _,
                        voltage_sensor: _,
                        current_sensor: _,
                        power_sensor: _,
                    } = self.scratch
                    else {
                        unreachable!()
                    };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    dev.read_block(
                        CommandCode::MFR_LOCATION as u8,
                        mfr_location,
                    )?;
                    dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                    dev.read_block(CommandCode::MFR_SERIAL as u8, mfr_serial)?;
                    dev.read_block(
                        CommandCode::MFR_FIRMWARE_DATA as u8,
                        mfr_firmware_data,
                    )?;
                    Ok(self.scratch)
                })
            }

            42 => {
                let (name, f, sensors) = by_refdes!(U352, isl68224);
                let dev = f(I2C.get_task_id());
                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                *self.scratch = InventoryData::Isl68224 {
                    mfr_id: [0u8; 4],
                    mfr_model: [0u8; 4],
                    mfr_revision: [0u8; 4],
                    mfr_date: [0u8; 4],
                    ic_device_id: [0u8; 4],
                    ic_device_rev: [0u8; 4],
                    voltage_sensors: SensorId::into_u32_array(sensors.voltage),
                    current_sensors: SensorId::into_u32_array(sensors.current),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::isl68224::CommandCode;
                    let InventoryData::Isl68224 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_date,
                        ic_device_id,
                        ic_device_rev,
                        voltage_sensors: _,
                        current_sensors: _,
                    } = self.scratch
                    else {
                        unreachable!()
                    };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
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
                    Ok(self.scratch)
                })
            }
            43 | 44 => {
                let (name, f, sensors) = match index - 43 {
                    0 => by_refdes!(U350, raa229618),
                    1 => by_refdes!(U351, raa229618),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());

                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                *self.scratch = InventoryData::Raa229618 {
                    mfr_id: [0u8; 4],
                    mfr_model: [0u8; 4],
                    mfr_revision: [0u8; 4],
                    mfr_date: [0u8; 4],
                    ic_device_id: [0u8; 4],
                    ic_device_rev: [0u8; 4],
                    temp_sensors: SensorId::into_u32_array(sensors.temperature),
                    power_sensors: SensorId::into_u32_array(sensors.power),
                    voltage_sensors: SensorId::into_u32_array(sensors.voltage),
                    current_sensors: SensorId::into_u32_array(sensors.current),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::raa229618::CommandCode;
                    let InventoryData::Raa229618 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_date,
                        ic_device_id,
                        ic_device_rev,
                        temp_sensors: _,
                        power_sensors: _,
                        voltage_sensors: _,
                        current_sensors: _,
                    } = self.scratch
                    else {
                        unreachable!()
                    };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
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
                    Ok(self.scratch)
                })
            }

            45..=49 => {
                let (name, f, sensors) = match index - 45 {
                    0 => by_refdes!(U522, tps546b24a),
                    1 => by_refdes!(U560, tps546b24a),
                    2 => by_refdes!(U524, tps546b24a),
                    3 => by_refdes!(U561, tps546b24a),
                    4 => by_refdes!(U565, tps546b24a),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());
                *self.scratch = InventoryData::Tps546b24a {
                    mfr_id: [0u8; 3],
                    mfr_model: [0u8; 3],
                    mfr_revision: [0u8; 3],
                    mfr_serial: [0u8; 3],
                    ic_device_id: [0u8; 6],
                    ic_device_rev: [0u8; 2],
                    nvm_checksum: 0u16,
                    temp_sensor: sensors.temperature.into(),
                    voltage_sensor: sensors.voltage.into(),
                    current_sensor: sensors.current.into(),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let InventoryData::Tps546b24a {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_serial,
                        ic_device_id,
                        ic_device_rev,
                        nvm_checksum,
                        temp_sensor: _,
                        voltage_sensor: _,
                        current_sensor: _,
                    } = self.scratch
                    else {
                        unreachable!()
                    };
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
                        nvm_checksum.as_mut_bytes(),
                    )?;
                    Ok(self.scratch)
                })
            }
            50 | 51 => {
                // U452 and U419, both ADM1272
                let (name, f, sensors) = match index - 50 {
                    0 => by_refdes!(U419, adm1272),
                    1 => by_refdes!(U452, adm1272),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());

                *self.scratch = InventoryData::Adm1272 {
                    mfr_id: [0u8; 3],
                    mfr_model: [0u8; 10],
                    mfr_revision: [0u8; 2],
                    mfr_date: [0u8; 6],

                    temp_sensor: sensors.temperature.into(),
                    voltage_sensor: sensors.voltage.into(),
                    current_sensor: sensors.current.into(),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::tps546b24a::CommandCode;
                    let InventoryData::Adm1272 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        mfr_date,
                        temp_sensor: _,
                        voltage_sensor: _,
                        current_sensor: _,
                    } = self.scratch
                    else {
                        unreachable!()
                    };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    dev.read_block(CommandCode::MFR_DATE as u8, mfr_date)?;
                    Ok(self.scratch)
                })
            }

            52..=57 => {
                let (mut name, f, sensors): ([u8; 7], _, _) = match index - 52 {
                    0 => by_refdes!(J194_U1, tmp117),
                    1 => by_refdes!(J195_U1, tmp117),
                    2 => by_refdes!(J196_U1, tmp117),
                    3 => by_refdes!(J197_U1, tmp117),
                    4 => by_refdes!(J198_U1, tmp117),
                    5 => by_refdes!(J199_U1, tmp117),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());

                // Convert the name from Jxxx_U1 (in the TOML file) -> Jxxx/U1
                // TODO(eliza): let's just get this from the include_refdes i2c
                // codegen...
                name[4] = b'/';

                *self.scratch = InventoryData::Tmp117 {
                    id: 0,
                    eeprom1: 0,
                    eeprom2: 0,
                    eeprom3: 0,
                    temp_sensor: sensors.temperature.into(),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    let InventoryData::Tmp117 {
                        id,
                        eeprom1,
                        eeprom2,
                        eeprom3,
                        temp_sensor: _,
                    } = self.scratch
                    else {
                        unreachable!();
                    };
                    *id = dev.read_reg(0x0Fu8)?;
                    *eeprom1 = dev.read_reg(0x05u8)?;
                    *eeprom2 = dev.read_reg(0x06u8)?;
                    *eeprom3 = dev.read_reg(0x08u8)?;
                    Ok(self.scratch)
                })
            }

            58 => {
                let (name, f, _sensors) = by_refdes!(U446, idt8a34003);
                let dev = f(I2C.get_task_id());
                *self.scratch = InventoryData::Idt8a34003 {
                    hw_rev: 0,
                    major_rel: 0,
                    minor_rel: 0,
                    hotfix_rel: 0,
                    product_id: 0,
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    let InventoryData::Idt8a34003 {
                        hw_rev,
                        major_rel,
                        minor_rel,
                        hotfix_rel,
                        product_id,
                    } = self.scratch
                    else {
                        unreachable!();
                    };
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
                    Ok(self.scratch)
                })
            }

            59 => {
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
            60..=70 => {
                let i = index - 60;
                let (mut name, _f, sensors) = match i {
                    0 => by_refdes!(J206_U8, max5970),
                    1 => by_refdes!(J207_U8, max5970),
                    2 => by_refdes!(J208_U8, max5970),
                    3 => by_refdes!(J209_U8, max5970),
                    4 => by_refdes!(J210_U8, max5970),
                    5 => by_refdes!(J211_U8, max5970),
                    6 => by_refdes!(J212_U8, max5970),
                    7 => by_refdes!(J213_U8, max5970),
                    8 => by_refdes!(J214_U8, max5970),
                    9 => by_refdes!(J215_U8, max5970),
                    10 => by_refdes!(U275, max5970, 7),
                    _ => panic!(),
                };
                // Transform Jxxx_U8 -> `Jxxx/U8`
                // TOD(eliza): get this from i2c device codegen
                if name[0] == b'J' {
                    name[4] = b'/';
                }
                *self.scratch = InventoryData::Max5970 {
                    voltage_sensors: SensorId::into_u32_array(sensors.voltage),
                    current_sensors: SensorId::into_u32_array(sensors.current),
                };
                self.tx_buf
                    .try_encode_inventory(sequence, &name, || Ok(self.scratch));
            }
            71 => {
                let (name, _f, sensors) = by_refdes!(U321, max31790);
                *self.scratch = InventoryData::Max31790 {
                    speed_sensors: SensorId::into_u32_array(sensors.speed),
                };
                self.tx_buf
                    .try_encode_inventory(sequence, &name, || Ok(self.scratch));
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
    /// Returns a designator (e.g. J206_U7) and constructor function
    fn get_sharkfin_vpd(i: usize) -> ([u8; 7], fn(TaskId) -> I2cDevice) {
        let (name, f, _sensors) = match i {
            0 => by_refdes!(J206_U7, at24csw080),
            1 => by_refdes!(J207_U7, at24csw080),
            2 => by_refdes!(J208_U7, at24csw080),
            3 => by_refdes!(J209_U7, at24csw080),
            4 => by_refdes!(J210_U7, at24csw080),
            5 => by_refdes!(J211_U7, at24csw080),
            6 => by_refdes!(J212_U7, at24csw080),
            7 => by_refdes!(J213_U7, at24csw080),
            8 => by_refdes!(J214_U7, at24csw080),
            9 => by_refdes!(J215_U7, at24csw080),
            _ => panic!("bad VPD index"),
        };
        (name, f)
    }

    fn dimm_inventory_lookup(&mut self, sequence: u64, index: u8) {
        // Build a name of the form `m{index}`, to match the designator
        let mut name = [0; 32];
        name[0] = b'M';
        if index >= 10 {
            name[1] = b'0' + (index / 10);
            name[2] = b'0' + (index % 10);
        } else {
            name[1] = b'0' + index;
        }

        let packrat = &self.packrat; // partial borrow
        *self.scratch = InventoryData::DimmSpd {
            id: [0u8; 512],
            temp_sensor: i2c_config::sensors::TSE2004AV_TEMPERATURE_SENSORS
                [index as usize]
                .into(),
        };
        self.tx_buf.try_encode_inventory(sequence, &name, || {
            // TODO: does packrat index match PCA designator?
            if packrat.get_spd_present(index) {
                let InventoryData::DimmSpd { id, .. } = self.scratch else {
                    unreachable!();
                };
                packrat.get_full_spd_data(index, id);
                Ok(self.scratch)
            } else {
                Err(InventoryDataResult::DeviceAbsent)
            }
        });
    }

    /// Reads the 128-bit unique ID from an AT24CSW080 EEPROM
    fn read_at24csw080_id(
        &mut self,
        sequence: u64,
        name: &[u8],
        f: fn(userlib::TaskId) -> I2cDevice,
    ) {
        *self.scratch = InventoryData::At24csw08xSerial([0u8; 16]);
        let dev = At24Csw080::new(f(I2C.get_task_id()));
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::At24csw08xSerial(id) = self.scratch else {
                unreachable!();
            };
            for (i, b) in id.iter_mut().enumerate() {
                *b = dev.read_security_register_byte(i as u8).map_err(|e| {
                    match e {
                        EepromError::I2cError(ResponseCode::NoDevice) => {
                            InventoryDataResult::DeviceAbsent
                        }
                        _ => InventoryDataResult::DeviceFailed,
                    }
                })?;
            }
            Ok(self.scratch)
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
        *self.scratch = InventoryData::VpdIdentity(Default::default());
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::VpdIdentity(identity) = self.scratch else {
                unreachable!();
            };
            *identity = read_one_barcode(dev, &[(*b"BARC", 0)])?.into();
            Ok(self.scratch)
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
        *self.scratch = InventoryData::FanIdentity {
            identity: Default::default(),
            vpd_identity: Default::default(),
            fans: Default::default(),
        };
        self.tx_buf.try_encode_inventory(sequence, name, || {
            let InventoryData::FanIdentity {
                identity,
                vpd_identity,
                fans: [fan0, fan1, fan2],
            } = self.scratch
            else {
                unreachable!();
            };
            *identity = read_one_barcode(dev, &[(*b"BARC", 0)])?.into();
            *vpd_identity =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 0)])?.into();
            *fan0 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 1)])?.into();
            *fan1 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 2)])?.into();
            *fan2 =
                read_one_barcode(dev, &[(*b"SASY", 0), (*b"BARC", 3)])?.into();
            Ok(self.scratch)
        })
    }
}

/// Free function to read a nested barcode, translating errors appropriately
fn read_one_barcode(
    dev: I2cDevice,
    path: &[([u8; 4], usize)],
) -> Result<oxide_barcode::VpdIdentity, InventoryDataResult> {
    let eeprom = At24Csw080::new(dev);
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
            Err(InventoryDataResult::DeviceAbsent)
        }
        Err(..) => Err(InventoryDataResult::DeviceFailed),
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
