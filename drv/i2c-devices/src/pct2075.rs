// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the PCT2075 temperature sensor

use crate::{TempSensor, Validate};
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Temp = 0x00,
    Conf = 0x01,
    Thyst = 0x02,
    Tos = 0x03,
    Tidle = 0x04,
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

pub struct Pct2075 {
    device: I2cDevice,
}

fn convert(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1 & 0b1110_0000;
    Celsius(f32::from(i16::from(msb) << 8 | i16::from(lsb)) / 256.0)
}

impl core::fmt::Display for Pct2075 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "pct2075: {}", &self.device)
    }
}

impl Pct2075 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }
}

impl TempSensor<Error> for Pct2075 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        match self.device.read_reg::<u8, [u8; 2]>(Register::Temp as u8) {
            Ok(buf) => Ok(convert((buf[0], buf[1]))),
            Err(code) => Err(Error::BadTempRead { code }),
        }
    }
}

impl Validate<ResponseCode> for Pct2075 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        let p = Pct2075::new(device);
        let t = p.read_temperature().map_err(ResponseCode::from)?;
        // Make sure the temperature reading is reasonable
        Ok(t.0 > 0.0 && t.0 < 100.0)
    }
}
