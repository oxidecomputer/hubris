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
//! - [`emc2305`]: EMC2305 fan driver
//! - [`isl68224`]: ISL68224 power controller
//! - [`lm5066`]: LM5066 hot swap controller
//! - [`lm5066i`]: LM5066I hot swap controller
//! - [`ltc4282`]: LTC4282 high current hot swap controller
//! - [`m24c02`]: M24C02 EEPROM, used in MWOCP68 power shelf
//! - [`m2_hp_only`]: M.2 drive; identical to `nvme_bmc`, with the limitation
//!   that communication is **only allowed** when the device is known to be
//!   powered (at the cost of locking up the I2C bus if you get it wrong).
//! - [`max5970`]: MAX5970 hot swap controller
//! - [`max6634`]: MAX6634 temperature sensor
//! - [`max31790`]: MAX31790 fan controller
//! - [`mcp9808`]: MCP9808 temperature sensor
//! - [`mwocp68`]: Murata power shelf
//! - [`nvme_bmc`]: NVMe basic management control
//! - [`pca9538`]: PCA9538 GPIO expander
//! - [`pca9956b`]: PCA9956B LED driver
//! - [`pct2075`]: PCT2075 temperature sensor
//! - [`raa229618`]: RAA229618 power controller
//! - [`raa229620a`]: RAA229620A power controller
//! - [`sbrmi10`]: AMD SB-RMI driver
//! - [`sbtsi`]: AMD SB-TSI temperature sensor
//! - [`tmp116`]: TMP116 temperature sensor
//! - [`tmp451`]: TMP451 temperature sensor
//! - [`tps546b24a`]: TPS546B24A buck converter
//! - [`tse2004av`]: TSE2004av SPD EEPROM with temperature sensor

#![no_std]

use drv_i2c_api::{I2cDevice, ResponseCode};
use pmbus::commands::CommandCode;

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

    ($device:expr, $dev:ident::$cmd:ident) => {{
        use $dev::$cmd;
        pmbus_read!($device, $cmd)
    }};
}

macro_rules! pmbus_rail_read {
    ($device:expr, $rail:expr, $cmd:ident) => {{
        let payload = [PAGE::CommandData::code(), $rail];

        match $cmd::CommandData::from_slice(&match $device
            .write_read_reg::<u8, [u8; $cmd::CommandData::len()]>(
                $cmd::CommandData::code(),
                &payload,
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
    }};

    ($device:expr, $rail:expr, $dev:ident::$cmd:ident) => {{
        use $dev::{$cmd, PAGE};
        pmbus_rail_read!($device, $rail, $cmd)
    }};
}

macro_rules! pmbus_rail_phase_read {
    ($device:expr, $rail:expr, $phase:expr, $cmd:ident) => {{
        let rail_payload = [PAGE::CommandData::code(), $rail];
        let phase_payload = [PHASE::CommandData::code(), $phase];

        match $cmd::CommandData::from_slice(&match $device
            .write_write_read_reg::<u8, [u8; $cmd::CommandData::len()]>(
                $cmd::CommandData::code(),
                &rail_payload,
                &phase_payload,
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
    }};
}

macro_rules! pmbus_write {
    ($device:expr, $cmd:ident) => {{
        let payload = [CommandCode::$cmd as u8];

        match $device.write(&payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: CommandCode::$cmd as u8,
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

    ($device:expr, $dev:ident::$cmd:ident, $data:expr) => {{
        use $dev::$cmd;
        pmbus_write!($device, $cmd, $data)
    }};
}

macro_rules! pmbus_rail_write {
    ($device:expr, $rail:expr, $cmd:ident, $data:expr) => {{
        let rpayload = [PAGE::CommandData::code(), $rail];

        let mut payload = [0u8; $cmd::CommandData::len() + 1];
        payload[0] = $cmd::CommandData::code();
        $data.to_slice(&mut payload[1..]);

        match $device.write_write(&rpayload, &payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: $cmd::CommandData::code(),
                code,
            }),
            Ok(_) => Ok(()),
        }
    }};

    ($device:expr, $rail:expr, $dev:ident::$cmd:ident, $data:expr) => {{
        use $dev::{$cmd, PAGE};
        pmbus_rail_write!($device, $rail, $cmd, $data)
    }};
}

struct BadValidation {
    cmd: u8,
    code: ResponseCode,
}

fn pmbus_validate<const N: usize>(
    device: &I2cDevice,
    cmd: CommandCode,
    expected: &[u8; N],
) -> Result<bool, BadValidation> {
    let mut id = [0u8; N];
    let cmd = cmd as u8;

    match device.read_block(cmd, &mut id) {
        Ok(size) => Ok(size == N && id == *expected),
        Err(code) => Err(BadValidation { cmd, code }),
    }
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

pub trait InputCurrentSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>>
{
    fn read_iin(&self) -> Result<userlib::units::Amperes, T>;
}

pub trait InputVoltageSensor<T: core::convert::Into<drv_i2c_api::ResponseCode>>
{
    fn read_vin(&self) -> Result<userlib::units::Volts, T>;
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
pub mod emc2305;
pub mod isl68224;
pub mod lm5066;
pub mod lm5066i;
pub mod ltc4282;
pub mod m24c02;
pub mod m2_hp_only;
pub mod max31790;
pub mod max5970;
pub mod max6634;
pub mod mcp9808;
pub mod mwocp68;
pub mod nvme_bmc;
pub mod pca9538;
pub mod pca9956b;
pub mod pct2075;
pub mod raa229618;
pub mod raa229620a;
pub mod sbrmi10;
pub mod sbtsi;
pub mod tmp117;
pub mod tmp451;
pub mod tps546b24a;
pub mod tse2004av;
