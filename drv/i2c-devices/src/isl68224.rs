use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Isl68224 {
    device: I2cDevice,
    mode: Option<pmbus::VOutModeCommandData>,
}

impl core::fmt::Display for Isl68224 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "isl68224: {}", &self.device)
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

impl Isl68224 {
    pub fn new(device: &I2cDevice) -> Self {
        Isl68224 {
            device: *device,
            mode: None,
        }
    }

    fn read_mode(&mut self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode {
            None => {
                let mode = pmbus_read!(self.device, VOUT_MODE)?;
                self.mode = Some(mode);
                mode
            }
            Some(mode) => mode,
        })
    }

    pub fn set_rail(&mut self, rail: u8) -> Result<(), Error> {
        let page = isl68224::PAGE::CommandData(rail);
        pmbus_write!(self.device, isl68224::PAGE, page)
    }

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, isl68224::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, isl68224::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
