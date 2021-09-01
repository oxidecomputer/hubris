//! Driver for the TPS546B24A buck converter

use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Tps546b24a {
    device: I2cDevice,
    mode: Option<VOutMode>,
}

impl core::fmt::Display for Tps546b24a {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tps546b24a: {}", &self.device)
    }
}

#[derive(Debug)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    InvalidData { err: pmbus::Error },
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err: err }
    }
}

impl Tps546b24a {
    pub fn new(device: &I2cDevice) -> Self {
        Tps546b24a {
            device: *device,
            mode: None,
        }
    }

    fn read_mode(&mut self) -> Result<VOutMode, Error> {
        Ok(match self.mode {
            None => {
                let mode = pmbus_read!(self.device, VOUT_MODE)?;
                self.mode = Some(mode);
                mode
            }
            Some(mode) => mode,
        })
    }

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, tps546b24a::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, tps546b24a::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
