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
use zerocopy::AsBytes;

use host_sp_messages::{InventoryData, InventoryDataResult};

userlib::task_slot!(I2C, i2c_driver);
userlib::task_slot!(SPI, spi_driver);

// SP_TO_SP5_CPU_INT_L
pub(crate) const SP_TO_HOST_CPU_INT_L: drv_stm32xx_sys_api::PinSet =
    drv_stm32xx_sys_api::Port::I.pin(7);
pub(crate) const SP_TO_HOST_CPU_INT_TYPE: drv_stm32xx_sys_api::OutputType =
    drv_stm32xx_sys_api::OutputType::PushPull;

impl ServerImpl {
    /// Number of devices in our inventory
    pub(crate) const INVENTORY_COUNT: u32 = 59;

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
            0 => {
                // U32/ID: SP barcode is available in packrat
                let packrat = &self.packrat;
                let mut data = InventoryData::VpdIdentity(Default::default());
                self.tx_buf.try_encode_inventory(sequence, b"U32/ID", || {
                    let InventoryData::VpdIdentity(identity) = &mut data else {
                        unreachable!();
                    };
                    *identity = packrat
                        .get_identity()
                        .map_err(|_| InventoryDataResult::DeviceAbsent)?
                        .into();
                    Ok(&data)
                });
            }
            1 => {
                // U32: Gimlet VPD EEPROM
                //
                // Note that for VPD AT24CSW080 identities, we allocate our
                // InventoryData in the outer frame then pass it in as a
                // reference; `read_at24csw080_id` typically isn't inlined, and
                // we're already paying a stack frame for the data in this
                // function, so it saves us 512 bytes of stack.
                let (name, f, _sensors) = by_refdes!(U32, at24csw080);
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(sequence, &name, f, &mut data)
            }
            2 => {
                // J34/ID: Fan VPD barcode (not available in packrat)
                self.read_fan_barcodes(
                    sequence,
                    b"J34/ID",
                    i2c_config::devices::at24csw080_fan_vpd,
                )
            }
            3 => {
                // J34: Fan VPD EEPROM (on the daughterboard)
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(
                    sequence,
                    b"J34/U1",
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
            // Sharkfin connectors start at J200 and are numbered sequentially
            4..=13 => {
                let (designator, f): ([u8; 4], _) =
                    Self::get_sharkfin_vpd(index as usize - 4);
                let mut name = *b"____/U7/ID";
                name[0..4].copy_from_slice(&designator);
                self.read_eeprom_barcode(sequence, &name, f)
            }
            14..=23 => {
                let (designator, f): ([u8; 4], _) =
                    Self::get_sharkfin_vpd(index as usize - 14);
                let mut name = *b"____/U7";
                name[0..4].copy_from_slice(&designator);
                let mut data = InventoryData::At24csw08xSerial([0u8; 16]);
                self.read_at24csw080_id(sequence, &name, f, &mut data)
            }
            24 => {
                // U20: the service processor itself
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
                    .try_encode_inventory(sequence, b"U20", || Ok(&data));
            }
            25 => {
                // U80: BMR491
                let (name, f, sensors) = by_refdes!(U80, bmr491);
                let dev = f(I2C.get_task_id());
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
                    } = &mut data
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
                    Ok(&data)
                })
            }
            26 => {
                let (name, f, sensors) = by_refdes!(U116, isl68224);
                let dev = f(I2C.get_task_id());
                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                let mut data = InventoryData::Isl68224 {
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
                    } = &mut data
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
                    Ok(&data)
                })
            }
            27..=28 => {
                let (name, f, sensors) = match index - 27 {
                    0 => by_refdes!(U90, raa229620a, 4),
                    1 => by_refdes!(U103, raa229620a),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());

                // To be stack-friendly, we declare our output here,
                // then bind references to all the member variables.
                let mut data = InventoryData::Raa229620a {
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
                    use pmbus::commands::raa229620a::CommandCode;
                    let InventoryData::Raa229620a {
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
                    } = &mut data
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
                    Ok(&data)
                })
            }
            29..=32 => {
                let (name, f, sensors) = match index - 29 {
                    0 => by_refdes!(U81, tps546b24a, 4),
                    1 => by_refdes!(U82, tps546b24a, 4),
                    2 => by_refdes!(U83, tps546b24a, 4),
                    3 => by_refdes!(U123, tps546b24a),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());
                let mut data = InventoryData::Tps546b24a {
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
                    } = &mut data
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
                        nvm_checksum.as_bytes_mut(),
                    )?;
                    Ok(&data)
                })
            }
            33 => {
                let (name, f, sensors) = by_refdes!(U79, adm1272);
                let dev = f(I2C.get_task_id());

                let mut data = InventoryData::Adm1272 {
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
                    } = &mut data
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
                    Ok(&data)
                })
            }
            34..=36 => {
                let (name, f, sensors) = match index - 34 {
                    0 => by_refdes!(U71, lm5066),
                    1 => by_refdes!(U72, lm5066),
                    2 => by_refdes!(U73, lm5066),
                    _ => unreachable!(),
                };
                let dev = f(I2C.get_task_id());
                let mut data = InventoryData::Lm5066 {
                    mfr_id: [0u8; 3],
                    mfr_model: [0u8; 8],
                    mfr_revision: [0u8; 2],

                    temp_sensor: sensors.temperature.into(),
                    power_sensor: sensors.temperature.into(),
                    voltage_sensor: sensors.voltage.into(),
                    current_sensor: sensors.current.into(),
                };
                self.tx_buf.try_encode_inventory(sequence, &name, || {
                    use pmbus::commands::lm5066::CommandCode;
                    let InventoryData::Lm5066 {
                        mfr_id,
                        mfr_model,
                        mfr_revision,
                        ..
                    } = &mut data
                    else {
                        unreachable!()
                    };
                    dev.read_block(CommandCode::MFR_ID as u8, mfr_id)?;
                    dev.read_block(CommandCode::MFR_MODEL as u8, mfr_model)?;
                    dev.read_block(
                        CommandCode::MFR_REVISION as u8,
                        mfr_revision,
                    )?;
                    Ok(&data)
                })
            }
            37..=42 => {
                let (connector_name, f, sensors): ([u8; 3], _, _) =
                    match index - 37 {
                        0 => by_refdes!(J44, tmp117),
                        1 => by_refdes!(J45, tmp117),
                        2 => by_refdes!(J46, tmp117),
                        3 => by_refdes!(J47, tmp117),
                        4 => by_refdes!(J48, tmp117),
                        5 => by_refdes!(J49, tmp117),
                        _ => unreachable!(),
                    };
                let dev = f(I2C.get_task_id());

                // Convert the name from Jxxx (in the TOML file) -> Jxxx/U1
                let mut name = *b"Jxx/U1";
                // All connector names should have length 3; that's checked by
                // the type in the tuple assignment above.
                name[..3].copy_from_slice(&connector_name);

                let mut data = InventoryData::Tmp117 {
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
                    } = &mut data
                    else {
                        unreachable!();
                    };
                    *id = dev.read_reg(0x0Fu8)?;
                    *eeprom1 = dev.read_reg(0x05u8)?;
                    *eeprom2 = dev.read_reg(0x06u8)?;
                    *eeprom3 = dev.read_reg(0x08u8)?;
                    Ok(&data)
                })
            }
            43 => {
                let spi = drv_spi_api::Spi::from(SPI.get_task_id());
                let ksz8463_dev = spi.device(drv_spi_api::devices::KSZ8463);
                let ksz8463 = ksz8463::Ksz8463::new(ksz8463_dev);
                let mut data = InventoryData::Ksz8463 { cider: 0 };
                self.tx_buf.try_encode_inventory(sequence, b"U37", || {
                    let InventoryData::Ksz8463 { cider } = &mut data else {
                        unreachable!();
                    };
                    *cider = ksz8463
                        .read(ksz8463::Register::CIDER)
                        .map_err(|_| InventoryDataResult::DeviceFailed)?;
                    Ok(&data)
                });
            }
            44..=55 => {
                let i = index - 44;
                let (name, _f, sensors) = match i {
                    0 => by_refdes!(J200, max5970),
                    1 => by_refdes!(J201, max5970),
                    2 => by_refdes!(J202, max5970),
                    3 => by_refdes!(J203, max5970),
                    4 => by_refdes!(J204, max5970),
                    5 => by_refdes!(J205, max5970),
                    6 => by_refdes!(J206, max5970),
                    7 => by_refdes!(J207, max5970),
                    8 => by_refdes!(J208, max5970),
                    9 => by_refdes!(J209, max5970),
                    10 => by_refdes!(U15, max5970, 4),
                    11 => by_refdes!(U54, max5970, 4),
                    _ => panic!(),
                };
                let data = InventoryData::Max5970 {
                    voltage_sensors: SensorId::into_u32_array(sensors.voltage),
                    current_sensors: SensorId::into_u32_array(sensors.current),
                };
                self.tx_buf
                    .try_encode_inventory(sequence, &name, || Ok(&data));
            }
            56 => {
                let (name, _f, sensors) = by_refdes!(U58, max31790);
                let data = InventoryData::Max31790 {
                    speed_sensors: SensorId::into_u32_array(sensors.speed),
                };
                self.tx_buf
                    .try_encode_inventory(sequence, &name, || Ok(&data));
            }
            57..=58 => {
                let (name, _f, sensors) = match index - 57 {
                    0 => by_refdes!(U42, ltc4282, 4),
                    1 => by_refdes!(U127, ltc4282),
                    _ => unreachable!(),
                };
                let data = InventoryData::Ltc4282 {
                    voltage_sensor: sensors.voltage.into(),
                    current_sensor: sensors.current.into(),
                };
                self.tx_buf
                    .try_encode_inventory(sequence, &name, || Ok(&data))
            }
            // TODO add DIMMS to inventory here?

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
    /// Returns a designator (e.g. J200) and constructor function
    fn get_sharkfin_vpd(i: usize) -> ([u8; 4], fn(TaskId) -> I2cDevice) {
        let (name, f, _sensors) = match i {
            0 => by_refdes!(J200, at24csw080),
            1 => by_refdes!(J201, at24csw080),
            2 => by_refdes!(J202, at24csw080),
            3 => by_refdes!(J203, at24csw080),
            4 => by_refdes!(J204, at24csw080),
            5 => by_refdes!(J205, at24csw080),
            6 => by_refdes!(J206, at24csw080),
            7 => by_refdes!(J207, at24csw080),
            8 => by_refdes!(J208, at24csw080),
            9 => by_refdes!(J209, at24csw080),
            _ => panic!("bad VPD index"),
        };
        (name, f)
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
            let InventoryData::At24csw08xSerial(id) = data else {
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
            Ok(data)
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
            let InventoryData::VpdIdentity(identity) = &mut data else {
                unreachable!();
            };
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
                fans: [fan0, fan1, fan2],
            } = &mut data
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
            Ok(&data)
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
