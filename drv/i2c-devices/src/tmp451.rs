// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the TMP451 temperature sensor

use crate::{TempSensor, Validate};
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    LocalTempHiByte = 0x00,
    RemoteTempHiByte = 0x01,
    Status = 0x02,
    Config = 0x03,
    ConverstionRate = 0x04,
    LocalTempHighLimit = 0x05,
    LocalTempLowLimit = 0x06,
    RemoteTempHighLimitHiByte = 0x07,
    RemoteTempLowLimitHiByte = 0x08,
    OneShotStart = 0x0F,
    RemoteTempLoByte = 0x10,
    RemoteTempOffsetHiByte = 0x11,
    RemoteTempOffsetLoByte = 0x12,
    RemoteTempHighLimitLoByte = 0x13,
    RemoteTempLowLimitLoByte = 0x14,
    LocalTempLoByte = 0x15,
    RemoteTempThermBLimit = 0x19,
    LocalTempThermBLimit = 0x20,
    ThermBHysteresis = 0x21,
    ConsecutiveAlertB = 0x22,
    EtaFactorCorrection = 0x23,
    DigitalFilterControl = 0x24,
    ManufacturerId = 0xFE,
}

#[derive(Debug)]
pub enum Error {
    BadRegisterRead { reg: Register, code: ResponseCode },
    BadRegisterWrite { reg: Register, code: ResponseCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRegisterRead { code, .. }
            | Error::BadRegisterWrite { code, .. } => code,
        }
    }
}

/// Selects whether this sensor reads the local or remote temperature
#[derive(Copy, Clone)]
pub enum Target {
    Local,
    Remote,
}

pub struct Tmp451 {
    device: I2cDevice,
    target: Target,
}

impl core::fmt::Display for Tmp451 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tmp451: {}", &self.device)
    }
}

impl Tmp451 {
    pub fn new(device: &I2cDevice, target: Target) -> Self {
        // By default, the chip runs at 16 conversions per second, which is
        // plenty fast for our use case.
        Self {
            device: *device,
            target,
        }
    }

    fn read_reg(&self, reg: Register) -> Result<u8, Error> {
        self.device
            .read_reg::<u8, u8>(reg as u8)
            .map_err(|code| Error::BadRegisterRead { reg, code })
    }
    pub fn write_reg(&self, reg: Register, value: u8) -> Result<(), Error> {
        self.device
            .write(&[reg as u8, value])
            .map_err(|code| Error::BadRegisterWrite { reg, code })
    }
}

impl Validate<Error> for Tmp451 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let id = Tmp451::new(device, Target::Local)
            .read_reg(Register::ManufacturerId)?;

        Ok(id == 0x55)
    }
}

impl TempSensor<Error> for Tmp451 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let (hi, lo) = match self.target {
            Target::Local => {
                (Register::LocalTempHiByte, Register::LocalTempLoByte)
            }
            Target::Remote => {
                (Register::RemoteTempHiByte, Register::RemoteTempLoByte)
            }
        };
        // Reading the high byte locks the low register byte until it is read
        let hi = self.read_reg(hi)?;
        let lo = self.read_reg(lo)?;
        Ok(Celsius(f32::from(hi) + f32::from(lo >> 4) * 0.0625f32))
    }
}
