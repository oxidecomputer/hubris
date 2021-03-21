//! Driver for the TMP116 temperature sensor

use crate::TempSensor;
use drv_i2c_api::*;
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq)]
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

#[derive(Debug)]
pub enum Error {
    BadRegisterRead { reg: Register, code: ResponseCode },
}

pub struct Tmp116 {
    pub i2c: I2c,
}

fn convert(raw: (u8, u8)) -> Celsius {
    Celsius(((((raw.0 as u16) << 8) | (raw.1 as u16)) as i16) as f32 / 128.0)
}

impl core::fmt::Display for Tmp116 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tmp116: {}", &self.i2c)
    }
}

impl Tmp116 {
    pub fn new(i2c: &I2c) -> Self {
        Self { i2c: *i2c }
    }

    fn read_reg(&self, reg: Register) -> Result<(u8, u8), Error> {
        match self.i2c.read_reg::<u8, [u8; 2]>(reg as u8) {
            Ok(buf) => Ok((buf[0], buf[1])),
            Err(code) => Err(Error::BadRegisterRead {
                reg: reg,
                code: code,
            }),
        }
    }
}

impl TempSensor<Error> for Tmp116 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        Ok(convert(self.read_reg(Register::TempResult)?))
    }
}
