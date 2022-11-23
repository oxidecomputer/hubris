// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP68-3600 Murata power shelf

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, Validate, VoltageSensor,
};
use core::cell::Cell;
use drv_i2c_api::*;
use pmbus::commands::mwocp68::*;
use pmbus::commands::CommandCode;
use pmbus::units::{Celsius, Rpm};
use pmbus::*;
use userlib::units::{Amperes, Volts};

pub struct Mwocp68 {
    device: I2cDevice,

    /// The index represents PMBus rail when reading voltage / current, and
    /// the sensor index when reading temperature (0-2) or fan speed (0-1).
    index: u8,

    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
    InvalidData { err: pmbus::Error },
}

impl From<BadValidation> for Error {
    fn from(value: BadValidation) -> Self {
        Self::BadValidation {
            cmd: value.cmd,
            code: value.code,
        }
    }
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            _ => ResponseCode::BadDeviceState,
        }
    }
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

impl Mwocp68 {
    pub fn new(device: &I2cDevice, index: u8) -> Self {
        Mwocp68 {
            device: *device,
            index,
            mode: Cell::new(None),
        }
    }

    fn set_rail(&self) -> Result<(), Error> {
        let page = PAGE::CommandData(self.index);
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

    pub fn read_temperature(&self) -> Result<Celsius, Error> {
        // Temperatures are accessible on all pages
        let r = match self.index {
            0 => pmbus_read!(self.device, READ_TEMPERATURE_1)?.get()?,
            1 => pmbus_read!(self.device, READ_TEMPERATURE_2)?.get()?,
            2 => pmbus_read!(self.device, READ_TEMPERATURE_3)?.get()?,
            _ => {
                return Err(Error::InvalidData {
                    err: pmbus::Error::InvalidCode,
                })
            }
        };
        Ok(r)
    }

    pub fn read_speed(&self) -> Result<Rpm, Error> {
        let r = match self.index {
            0 => pmbus_read!(self.device, READ_FAN_SPEED_1)?.get()?,
            1 => pmbus_read!(self.device, READ_FAN_SPEED_2)?.get()?,
            _ => {
                return Err(Error::InvalidData {
                    err: pmbus::Error::InvalidCode,
                })
            }
        };
        Ok(r)
    }
}

impl Validate<Error> for Mwocp68 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"MWOCP68-3600-D-RM";
        pmbus_validate(device, CommandCode::MFR_MODEL, expected)
            .map_err(Into::into)
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
