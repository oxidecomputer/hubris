// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! I2C device drivers
//!
//! This crate contains (generally) all I2C device drivers, including:
//!
//! - [`adm1272`]: ADM1272 hot swap controller
//! - [`adt7420`]: ADT7420 temperature sensor
//! - [`at24csw080`]: AT24CSW080 serial EEPROM
//! - [`ds2482`]: DS2482-100 1-wire initiator
//! - [`isl68224`]: ISL68224 power controller
//! - [`max6634`]: MAX6634 temperature sensor
//! - [`max31790`]: MAX31790 fan controller
//! - [`mcp9808`]: MCP9808 temperature sensor
//! - [`pca9538`]: PCA9538 GPIO expander
//! - [`pct2075`]: PCT2075 temperature sensor
//! - [`raa229618`]: RAA229618 power controller
//! - [`sbtsi`]: AMD SB-TSI temperature sensor
//! - [`tmp116`]: TMP116 temperature sensor
//! - [`tmp451`]: TMP451 temperature sensor
//! - [`tps546b24a`]: TPS546B24A buck converter
//! - [`tse2004av`]: TSE2004av SPD EEPROM with temperature sensor

#![no_std]

macro_rules! pmbus_read {
    ($device:expr, $cmd:ident) => {
        match $cmd::CommandData::from_slice(&match $device
            .read_reg::<u8, [u8; $cmd::CommandData::len()]>(
                $cmd::CommandData::code(),
            ) {
            Ok(rval) => Ok(rval),
            Err(code) => Err(Error::BadRead {
                cmd: $cmd::CommandData::code(),
                code,
            }),
        }?) {
            Some(data) => Ok(data),
            None => Err(Error::BadData {
                cmd: $cmd::CommandData::code(),
            }),
        }
    };

    ($device:expr, $dev:ident::$cmd:ident) => {
        match $dev::$cmd::CommandData::from_slice(&match $device
            .read_reg::<u8, [u8; $dev::$cmd::CommandData::len()]>(
                $dev::$cmd::CommandData::code(),
            ) {
            Ok(rval) => Ok(rval),
            Err(code) => Err(Error::BadRead {
                cmd: $dev::$cmd::CommandData::code(),
                code,
            }),
        }?) {
            Some(data) => Ok(data),
            None => Err(Error::BadData {
                cmd: $dev::$cmd::CommandData::code(),
            }),
        }
    };
}

macro_rules! pmbus_write {
    ($device:expr, $dev:ident::$cmd:ident, $data:expr) => {{
        let mut payload = [0u8; $dev::$cmd::CommandData::len() + 1];
        payload[0] = $dev::$cmd::CommandData::code();
        $data.to_slice(&mut payload[1..]);

        match $device.write(&payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: $dev::$cmd::CommandData::code(),
                code,
            }),
            Ok(_) => Ok(()),
        }
    }};

    ($device:expr, $cmd:ident, $data:expr) => {{
        let mut payload = [0u8; $cmd::CommandData::len() + 1];
        payload[0] = $cmd::CommandData::code();
        $data.to_slice(&mut payload[1..]);

        match $device.write(&payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: $cmd::CommandData::code(),
                code,
            }),
            Ok(_) => Ok(()),
        }
    }};
}

macro_rules! pmbus_validate {
    ($device:expr, $dev:ident::$cmd:ident, $expected:ident) => {{
        let mut id = [0u8; 16];

        match $device.read_block::<u8>($dev::CommandCode::$cmd as u8, &mut id) {
            Ok(size) => {
                Ok(size == $expected.len()
                    && id[0..$expected.len()] == $expected)
            }
            Err(code) => Err(Error::BadValidation {
                cmd: CommandCode::$cmd as u8,
                code,
            }),
        }
    }};

    ($device:expr, $cmd:ident, $expected:ident) => {{
        let mut id = [0u8; 16];

        match $device.read_block::<u8>(CommandCode::$cmd as u8, &mut id) {
            Ok(size) => {
                Ok(size == $expected.len()
                    && id[0..$expected.len()] == $expected)
            }
            Err(code) => Err(Error::BadValidation {
                cmd: CommandCode::$cmd as u8,
                code,
            }),
        }
    }};
}

pub trait TempSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>> {
    fn read_temperature(&self) -> Result<userlib::units::Celsius, T>;
}

pub trait PowerSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>> {
    fn read_power(&mut self) -> Result<userlib::units::Watts, T>;
}

pub trait CurrentSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>> {
    fn read_iout(&self) -> Result<userlib::units::Amperes, T>;
}

pub trait VoltageSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>> {
    fn read_vout(&self) -> Result<userlib::units::Volts, T>;
}

pub trait Validate<T: core::convert::Into<drv_i2c_api::ResponseCode>> {
    //
    // We have a default implementation that returns false to allow for
    // drivers to be a little more easily developed -- but it is expected
    // that each driver will provide a proper implementation that validates
    // the device.
    //
    fn validate(_device: &drv_i2c_api::I2cDevice) -> Result<bool, T> {
        Ok(false)
    }
}

pub mod adm1272;
pub mod adt7420;
pub mod at24csw080;
pub mod bmr491;
pub mod ds2482;
pub mod isl68224;
pub mod max31790;
pub mod max6634;
pub mod mcp9808;
pub mod pca9538;
pub mod pct2075;
pub mod raa229618;
pub mod sbtsi;
pub mod tmp117;
pub mod tmp451;
pub mod tps546b24a;
pub mod tse2004av;
