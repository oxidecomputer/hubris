//! Driver for the MCP9808 temperature sensor

use crate::TempSensor;
use drv_i2c_api::*;
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

pub struct Mcp9808 {
    pub i2c: I2c,
}

fn convert(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1;
    Celsius(((((msb as u16) << 11) | (lsb as u16) << 3) as i16) as f32 / 128.0)
}

impl core::fmt::Display for Mcp9808 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "mcp9808: {}", &self.i2c)
    }
}

impl Mcp9808 {
    pub fn new(i2c: &I2c) -> Self {
        Self { i2c: *i2c }
    }
}

impl TempSensor<Error> for Mcp9808 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        match self
            .i2c
            .read_reg::<u8, [u8; 2]>(Register::Temperature as u8)
        {
            Ok(buf) => Ok(convert((buf[0], buf[1]))),
            Err(code) => Err(Error::BadTempRead { code: code }),
        }
    }
}
