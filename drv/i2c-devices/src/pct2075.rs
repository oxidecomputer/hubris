//! Driver for the PCT2075 temperature sensor

use crate::TempSensor;
use drv_i2c_api::*;
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq)]
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

pub struct Pct2075 {
    pub i2c: I2c,
}

fn convert(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1 & 0b1110_0000;

    Celsius(((((msb as u16) << 8) | (lsb as u16)) as i16) as f32 / 256.0)
}

impl core::fmt::Display for Pct2075 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "pct2075: {}", &self.i2c)
    }
}

impl Pct2075 {
    pub fn new(i2c: &I2c) -> Self {
        Self { i2c: *i2c }
    }
}

impl TempSensor<Error> for Pct2075 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        match self.i2c.read_reg::<u8, [u8; 2]>(Register::Temp as u8) {
            Ok(buf) => Ok(convert((buf[0], buf[1]))),
            Err(code) => Err(Error::BadTempRead { code: code }),
        }
    }
}
