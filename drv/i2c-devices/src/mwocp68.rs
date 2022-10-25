// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP68-??? Murata power shelf

use crate::{CurrentSensor, Validate, VoltageSensor};
use core::cell::Cell;
use drv_i2c_api::*;
use pmbus::commands::mwocp68::*;
use pmbus::commands::CommandCode;
use pmbus::*;
use userlib::units::*;

pub struct Mwocp68 {
    device: I2cDevice,
    rail: u8,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

#[derive(Debug)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
    InvalidData { err: pmbus::Error },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            _ => panic!(),
        }
    }
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

impl Mwocp68 {
    pub fn new(device: &I2cDevice, rail: u8) -> Self {
        Mwocp68 {
            device: *device,
            rail,
            mode: Cell::new(None),
        }
    }

    fn set_rail(&self) -> Result<(), Error> {
        let page = PAGE::CommandData(self.rail);
        pmbus_write!(self.device, PAGE, page)
    }

    fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode.get() {
            None => {
                let mode = pmbus_read!(self.device, commands::VOUT_MODE)?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }

    pub fn read_temperature(&self, i: usize) -> Result<Celsius, ResponseCode> {
        todo!()
    }

    pub fn read_speed(&self, i: usize) -> Result<Celsius, ResponseCode> {
        todo!()
    }
}

impl Validate<Error> for Mwocp68 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = *b"MWOCP68-3600-D-RM";
        pmbus_validate!(device, MFR_MODEL, expected)
    }
}

impl VoltageSensor<Error> for Mwocp68 {
    fn read_vout(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vout = pmbus_read!(self.device, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl CurrentSensor<Error> for Mwocp68 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iout = pmbus_read!(self.device, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
