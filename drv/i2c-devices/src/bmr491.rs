// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the BMR491 IBC

use crate::TempSensor;
use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Bmr491 {
    device: I2cDevice,
    mode: Option<pmbus::VOutModeCommandData>,
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

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            _ => panic!(),
        }
    }
}

impl Bmr491 {
    pub fn new(device: &I2cDevice) -> Self {
        Bmr491 {
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

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, bmr491::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, bmr491::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl TempSensor<Error> for Bmr491 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, bmr491::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}
