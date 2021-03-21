//! Driver for the ADT7420 temperature sensor

use crate::TempSensor;
use drv_i2c_api::*;
use userlib::units::*;

const ADT7420_ID: u8 = 0xcb;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Register {
    TempMSB = 0x00,
    TempLSB = 0x01,
    Status = 0x02,
    Configuration = 0x03,
    THighMSB = 0x04,
    THighLSB = 0x05,
    TLowMSB = 0x06,
    TLowLSB = 0x07,
    TCritMSB = 0x08,
    TCritLSB = 0x09,
    THyst = 0x0a,
    ID = 0x0b,
}

#[derive(Debug)]
pub enum Error {
    BadID { id: u8 },
    BadValidate { code: ResponseCode },
    BadTempRead { code: ResponseCode },
}

pub struct Adt7420 {
    pub i2c: I2c,
}

//
// Converts a tuple of two u8s (an MSB and an LSB) comprising a 13-bit value
// into a signed, floating point Celsius temperature value.  (This has been
// validated and verified against the sample data in Table 5 of the ADT7420
// datasheet.)
//
fn convert_temp13(raw: (u8, u8)) -> Celsius {
    let msb = raw.0;
    let lsb = raw.1;
    let val = ((msb & 0x7f) as u16) << 5 | ((lsb >> 3) as u16);

    Celsius(if msb & 0b1000_0000 != 0 {
        (val as i16 - 4096) as f32 / 16.0
    } else {
        val as f32 / 16.0
    })
}

impl core::fmt::Display for Adt7420 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adt7420: {}", &self.i2c)
    }
}

impl Adt7420 {
    pub fn new(i2c: &I2c) -> Self {
        Self { i2c: *i2c }
    }

    pub fn validate(&self) -> Result<(), Error> {
        match self.i2c.read_reg::<u8, u8>(Register::ID as u8) {
            Ok(id) if id == ADT7420_ID => Ok(()),
            Ok(id) => Err(Error::BadID { id: id }),
            Err(code) => Err(Error::BadValidate { code: code }),
        }
    }
}

impl TempSensor<Error> for Adt7420 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        match self.i2c.read_reg::<u8, [u8; 2]>(Register::TempMSB as u8) {
            Ok(buf) => Ok(convert_temp13((buf[0], buf[1]))),
            Err(code) => Err(Error::BadTempRead { code: code }),
        }
    }
}
