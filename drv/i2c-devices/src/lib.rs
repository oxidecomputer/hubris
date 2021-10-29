//! I2C device drivers
//!
//! This crate contains (generally) all I2C device drivers, including:
//!
//! - [`adm1272`]: ADM1272 hot swap controller
//! - [`adt7420`]: ADT7420 temperature sensor
//! - [`ds2482`]: DS2482-100 1-wire initiator
//! - [`max6634`]: MAX6634 temperature sensor
//! - [`max31790`]: MAX31790 fan controller
//! - [`mcp9808`]: MCP9808 temperature sensor
//! - [`pct2075`]: PCT2075 temperature sensor
//! - [`tmp116`]: TMP116 temperature sensor
//! - [`tps546b24a`]: TPS546B24A buck converter

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
                code: code,
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
                code: code,
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
                code: code,
            }),
            Ok(_) => Ok(()),
        }
    }};
}

pub trait TempSensor<T> {
    fn read_temperature(&self) -> Result<userlib::units::Celsius, T>;
}

pub mod adm1272;
pub mod adt7420;
pub mod ds2482;
pub mod max31790;
pub mod max6634;
pub mod mcp9808;
pub mod pct2075;
pub mod tmp116;
pub mod tps546b24a;
