// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the MAX6634 temperature sensor

use crate::{TempSensor, Validate};
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Register {
    Temperature = 0x00,
    Configuration = 0x01,
    THyst = 0x02,
    TMax = 0x03,
    TLow = 0x04,
    THigh = 0x05,
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

pub struct Max6634 {
    device: I2cDevice,
}

//
// Converts a tuple of two u8s (an MSB and an LSB) comprising a 13-bit value
// into a signed, floating point Celsius temperature value.  Note that the
// sample data in Table 6 of the data sheet has two errors in it:
//
//   BINARY VALUE           HEX VALUE     DATASHEET     CORRECTED
//   1111 0011 0111 0XXX       0xf370       -25.000       -25.125
//   1110 0100 0111 0XXX       0xe470       -55.000       -55.125
//
// It should go without saying that this driver does the correct conversion,
// not the one implied by the erroneous datasheet.  (It should also go without
// saying that -25 degrees C is really damned cold, and unlikely to be a value
// that we would ever pull off of a sensor.)
//
fn convert(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1 & 0b1111_1000;
    Celsius(f32::from(i16::from(msb) << 8 | i16::from(lsb)) / 128.0)
}

impl core::fmt::Display for Max6634 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "max6634: {}", &self.device)
    }
}

impl Max6634 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }
}

impl TempSensor<Error> for Max6634 {
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

impl Validate<Error> for Max6634 {}
