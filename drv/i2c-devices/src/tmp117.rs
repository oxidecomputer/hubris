// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the TMP117 temperature sensor

use crate::{TempSensor, Validate};
use drv_i2c_api::*;
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    TempResult = 0x00,
    Configuration = 0x01,
    THighLimit = 0x02,
    TLowLimit = 0x03,
    EEPROMUnlock = 0x04,
    EEPROM1 = 0x05,
    EEPROM2 = 0x06,
    TempOffset = 0x07,
    EEPROM3 = 0x08,
    DeviceID = 0x0f,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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

pub struct Tmp117 {
    device: I2cDevice,
}

fn convert(raw: u16) -> Celsius {
    Celsius(f32::from(raw as i16) / 128.0)
}

impl core::fmt::Display for Tmp117 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tmp117: {}", &self.device)
    }
}

impl Tmp117 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn read_reg(&self, reg: Register) -> Result<u16, Error> {
        self.read_reg_bytes(reg).map(u16::from_le_bytes)
    }

    fn read_reg_bytes(&self, reg: Register) -> Result<[u8; 2], Error> {
        self.device
            .read_reg::<u8, [u8; 2]>(reg as u8)
            .map_err(|code| Error::BadRegisterRead { reg, code })
    }

    pub fn read_eeprom(&self) -> Result<[u8; 6], Error> {
        let [ee1_0, ee1_1] = self.read_reg_bytes(Register::EEPROM1)?;
        let [ee2_0, ee2_1] = self.read_reg_bytes(Register::EEPROM2)?;
        let [ee3_0, ee3_1] = self.read_reg_bytes(Register::EEPROM3)?;

        Ok([ee1_0, ee1_1, ee2_0, ee2_1, ee3_0, ee3_1])
    }
}

impl Validate<Error> for Tmp117 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let id = Tmp117::new(device).read_reg(Register::DeviceID)?;

        Ok(id == 0x0117)
    }
}

impl TempSensor<Error> for Tmp117 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        Ok(convert(self.read_reg(Register::TempResult)?))
    }
}
