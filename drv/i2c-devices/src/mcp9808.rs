// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the MCP9808 temperature sensor

use crate::TempSensor;
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::*;

pub enum Register {
    Reserved = 0b000,
    Config = 0b0001,
    TUpper = 0b0010,
    TLower = 0b0011,
    TCrit = 0b0100,
    Temperature = 0b0101,
    ManufaturerID = 0b0110,
    DeviceID = 0b0111,
}

#[derive(Debug)]
pub enum Error {
    BadTempRead { code: ResponseCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadTempRead { code } => code,
        }
    }
}

pub struct Mcp9808 {
    device: I2cDevice,
}

fn convert(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1;
    Celsius(f32::from(i16::from(msb) << 11 | (i16::from(lsb) << 3)) / 128.0)
}

impl core::fmt::Display for Mcp9808 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "mcp9808: {}", &self.device)
    }
}

impl Mcp9808 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }
}

impl TempSensor<Error> for Mcp9808 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        match self
            .device
            .read_reg::<u8, [u8; 2]>(Register::Temperature as u8)
        {
            Ok(buf) => Ok(convert((buf[0], buf[1]))),
            Err(code) => Err(Error::BadTempRead { code }),
        }
    }
}
