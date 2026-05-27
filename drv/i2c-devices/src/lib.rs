// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! I2C device drivers
//!
//! This crate contains (generally) all I2C device drivers, including:
//!
//! - [`adm127x`]: ADM1272 or ADM1273 hot swap controller
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
//! - [`sbrmi`]: AMD SB-RMI driver
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
        $device
            .read_reg::<u8, [u8; $cmd::CommandData::len()]>(
                $cmd::CommandData::code(),
            )
            .map_err(|code| Error::BadRead {
                cmd: $cmd::CommandData::code(),
                code,
            })
            .and_then(|rval| {
                $cmd::CommandData::from_slice(&rval).ok_or(Error::BadData {
                    cmd: $cmd::CommandData::code(),
                })
            })
    };

    ($device:expr, $dev:ident::$cmd:ident) => {{
        use $dev::$cmd;
        pmbus_read!($device, $cmd)
    }};
}

macro_rules! pmbus_rail_read {
    (@raw => $device:expr, $rail:expr, $cmd_code:expr, $len:expr) => {
        $device
            .write_read_reg::<u8, [u8; $len]>(
                $cmd_code,
                &[PAGE::CommandData::code(), $rail],
            )
            .map_err(|code| Error::BadRead {
                cmd: $cmd_code,
                code,
            })
    };

    ($device:expr, $rail:expr, $cmd:ident) => {{
        let cmd_code = $cmd::CommandData::code();
        const CMD_LEN: usize = $cmd::CommandData::len();

        pmbus_rail_read!(@raw => $device, $rail, cmd_code, CMD_LEN)
            .and_then(|rval| {
                $cmd::CommandData::from_slice(&rval).ok_or(Error::BadData {
                    cmd: $cmd::CommandData::code(),
                })
            })
    }};

    ($device:expr, $rail:expr, $dev:ident::$cmd:ident) => {{
        use $dev::{PAGE, $cmd};
        pmbus_rail_read!($device, $rail, $cmd)
    }};
}

macro_rules! pmbus_rail_phase_read {
    ($device:expr, $rail:expr, $phase:expr, $cmd:ident) => {
        $device
            .write_write_read_reg::<u8, [u8; $cmd::CommandData::len()]>(
                $cmd::CommandData::code(),
                &[PAGE::CommandData::code(), $rail],
                &[PHASE::CommandData::code(), $phase],
            )
            .map_err(|code| Error::BadRead {
                cmd: $cmd::CommandData::code(),
                code,
            })
            .and_then(|rval| {
                $cmd::CommandData::from_slice(&rval).ok_or(Error::BadData {
                    cmd: $cmd::CommandData::code(),
                })
            })
    };
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
    // Write a command with no additional data bytes.
    ($device:expr, $rail:expr, $cmd:ident) => {{
        let rpayload = [PAGE::CommandData::code(), $rail];
        let payload: [u8; 1] = [CommandCode::$cmd as u8];
        match $device.write_write(&rpayload, &payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: CommandCode::$cmd as u8,
                code,
            }),
            Ok(_) => Ok(()),
        }
    }};
    // Write a command code followed by data.
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
        use $dev::{PAGE, $cmd};
        pmbus_rail_write!($device, $rail, $cmd, $data)
    }};
}

/// Write the mask `$mask` to the `SMBALERT_MASK` register for `$reg`, where
/// `$reg` is a status register, and `$mask` is a `CommandData` value for that
/// register.
///
/// Importantly, `$reg` must be a PMBus `STATUS_<whatever>` register. This macro
/// cannot stop you from providing any `CommandCode` as the value of `$reg` and
/// any `CommandData` as the value of `$mask`, but, uh, don't do that. On the
/// other hand, the macro *does* at least ensure that `$mask` is a `CommandData`.
/// for the same register as `$reg`.
macro_rules! pmbus_smbalert_mask_write {
    ($device:expr, $rail:expr, $reg:ident, $mask:expr) => {{
        // This assignment is just a type assertion that `$mask` is a
        // `CommandData` for the same register as `$reg`.
        let mask: $reg::CommandData = $mask;
        let rpayload = [PAGE::CommandData::code(), $rail];
        // N.B. that the status register *should* always be a single byte, but
        // we'll do this "properly" just in case.
        let mut payload = [0u8; $reg::CommandData::len() + 2];
        // 0               7               15              23
        // +---------------+---------------+---------------+
        // | SMBALERT_MASK | register code | mask byte     |
        // +---------------+---------------+---------------+
        payload[0] = CommandCode::SMBALERT_MASK as u8;
        payload[1] = $reg::CommandData::code();
        mask.to_slice(&mut payload[2..]);

        match $device.write_write(&rpayload, &payload) {
            Err(code) => Err(Error::BadWrite {
                cmd: CommandCode::SMBALERT_MASK as u8,
                code,
            }),
            Ok(_) => Ok(()),
        }
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

// grumble grumble, copied from `gateway_messages::sp_to_mgs::PmbusStatus`
// grumble grumble, also basically the same as `ereports/src/pwr`
pub struct PmbusStatus {
    pub status_word: u16,
    pub status_vout: u8,
    pub status_iout: u8,
    pub status_temperature: u8,
    pub status_cml: u8,
    pub status_other: u8,
    pub status_input: u8,
    pub status_mfr_specific: u8,
    pub status_fans_1_2: u8,
    pub status_fans_3_4: u8,
}

pub enum PmbusStatusError {
    BadRead { cmd: u8, code: ResponseCode },
    BadData { cmd: u8, },
}

impl PmbusStatus {
    pub fn read_from(dev: &I2cDevice, rail_idx: u8) -> Result<Self, ()> {
        use pmbus::commands::*;
        use PmbusStatusError as Error;

        Ok(PmbusStatus {
            status_word: pmbus_rail_read!(dev, rail_idx, STATUS_WORD).map_err(drop)?.0,
            status_vout: pmbus_rail_read!(dev, rail_idx, STATUS_VOUT).map_err(drop)?.0,
            status_iout: pmbus_rail_read!(dev, rail_idx, STATUS_IOUT).map_err(drop)?.0,
            status_temperature: pmbus_rail_read!(dev, rail_idx, STATUS_TEMPERATURE).map_err(drop)?.0,
            status_cml: pmbus_rail_read!(dev, rail_idx, STATUS_CML).map_err(drop)?.0,
            status_other: pmbus_rail_read!(dev, rail_idx, STATUS_OTHER).map_err(drop)?.0,
            status_input: pmbus_rail_read!(dev, rail_idx, STATUS_INPUT).map_err(drop)?.0,
            status_fans_1_2: pmbus_rail_read!(dev, rail_idx, STATUS_FANS_1_2).map_err(drop)?.0,
            status_fans_3_4: pmbus_rail_read!(dev, rail_idx, STATUS_FANS_3_4).map_err(drop)?.0,

            // Unfortunately, STATUS_MFR_SPECIFIC *is* defined in the pmbus crate, but doesn't have a
            // "structured" representation, so instead use a raw representation. It *could* be argued
            // that since we're not actually peeking at any of the introspection stuff, we could do
            // the same for all the items above, and save ourselved a little indirection, but for now
            // just hole-punch the minimum amount necessary
            status_mfr_specific: pmbus_rail_read!(@raw => dev, rail_idx, CommandCode::STATUS_MFR_SPECIFIC as u8, 1).map_err(drop)?[0],
        })
    }
}

pub mod adm127x;
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
pub mod sbrmi;
pub mod sbtsi;
pub mod tmp117;
pub mod tmp451;
pub mod tps546b24a;
pub mod tse2004av;
