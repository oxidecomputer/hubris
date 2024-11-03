// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for any chip implementing the TSE2004av specification, which is used
//! for SPD (serial presence detection) and temperature sensing on DIMMs.

use crate::TempSensor;
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Capabilities = 0x00,
    Configuration = 0x01,
    HighLimit = 0x02,
    LowLimit = 0x03,
    TcritLimit = 0x04,
    AmbientTemp = 0x05,
    ManufacturerId = 0x06,
    DeviceIdRevision = 0x07,
}

#[derive(Debug)]
pub enum Error {
    BadRegisterRead { reg: Register, code: ResponseCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRegisterRead { code, .. } => code,
        }
    }
}

pub struct Tse2004Av {
    device: I2cDevice,
}

impl core::fmt::Display for Tse2004Av {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TSE2004av: {}", &self.device)
    }
}

impl Tse2004Av {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    fn read_reg(&self, reg: Register) -> Result<u16, Error> {
        self.device
            .read_reg::<u8, u16>(reg as u8)
            .map_err(|code| Error::BadRegisterRead { reg, code })
            .map(u16::from_be)
    }
}

impl TempSensor<Error> for Tse2004Av {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let t: u16 = self.read_reg(Register::AmbientTemp)?;

        // The actual temperature is a 13-bit two's complement value.
        //
        // We shift it so that the sign bit is in the right place, cast it
        // to an i16 to make it signed, then scale it into a float.
        let t = (t << 3) as i16;
        Ok(Celsius(f32::from(t) * 0.0078125f32))
    }
}

impl crate::Validate<Error> for Tse2004Av {
    fn validate(device: &drv_i2c_api::I2cDevice) -> Result<bool, Error> {
        let dev = Tse2004Av::new(device);
        let r = dev.read_reg(Register::DeviceIdRevision)?;
        // "The upper byte of the Device ID / Revision Register must be 0x22
        //  for the TSE2004av"  --EE1004 and TSE2004 Device Specifications
        Ok((r >> 8) == 0x22)
    }
}
