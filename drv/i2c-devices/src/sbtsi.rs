// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for AMD SB-TSI interface

use crate::TempSensor;
use drv_i2c_api::*;
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Register {
    CpuTempInt = 0x01,
    Status = 0x02,
    Config = 0x03,
    UpdateRate = 0x04,
    HiTempInt = 0x07,
    LoTempInt = 0x08,
    ConfigWr = 0x09,
    CpuTempDec = 0x10,
    CpuTempOffInt = 0x11,
    CpuTempOffDec = 0x12,
    HiTempDec = 0x13,
    LoTempDec = 0x14,
    TimeoutConfig = 0x22,
    AlertThreshold = 0x32,
    AlertConfig = 0xbf,
    ManId = 0xfe,
    Revision = 0xff,
}

#[derive(Debug)]
pub enum Error {
    BadRegisterRead { reg: Register, code: ResponseCode },
}

pub struct SbTsi {
    device: I2cDevice,
}

fn convert(i: u8, d: u8) -> Celsius {
    Celsius(f32::from(i) + (f32::from(d >> 5) / 8.0))
}

impl core::fmt::Display for SbTsi {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "sbtsi: {}", &self.device)
    }
}

impl SbTsi {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    fn read_reg(&self, reg: Register) -> Result<u8, Error> {
        match self.device.read_reg::<u8, u8>(reg as u8) {
            Ok(buf) => Ok(buf),
            Err(code) => Err(Error::BadRegisterRead {
                reg: reg,
                code: code,
            }),
        }
    }
}

impl TempSensor<Error> for SbTsi {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        // Reading the integer portion latches the decimal portion; we need
        // to read it first, and then immediately read the decimal portion.
        let i = self.read_reg(Register::CpuTempInt)?;
        let d = self.read_reg(Register::CpuTempDec)?;

        Ok(convert(i, d))
    }
}
