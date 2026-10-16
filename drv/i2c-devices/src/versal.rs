// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the AMD Versal SYSMON interface, as specified by AMD reference
//! document AM006.

use core::cell::Cell;

use crate::{
    BadValidation, TempSensor, Validate, VoltageSensor, pmbus_validate,
};
use drv_i2c_api::*;
use pmbus::{
    CommandCode,
    commands::{VOUT_MODE, versal},
};
use userlib::units::*;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// I2C error on PMBus read from device
    BadRead { cmd: u8, code: ResponseCode },

    /// I2C error on PMBus write to device
    BadWrite { cmd: u8, code: ResponseCode },

    /// Failed to parse PMBus data from device
    BadData { cmd: u8 },

    /// I2C error attempting to validate device
    BadValidation { cmd: u8, code: ResponseCode },

    /// PMBus data returned from device is invalid
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

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            Error::BadData { .. } | Error::InvalidData { .. } => {
                ResponseCode::BadDeviceState
            }
        }
    }
}

pub struct Versal {
    device: I2cDevice,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

impl core::fmt::Display for Versal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "versal: {}", &self.device)
    }
}

impl Versal {
    pub fn new(device: &I2cDevice) -> Self {
        Self {
            device: *device,
            mode: Cell::new(None),
        }
    }

    pub fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode.get() {
            None => {
                let mode = pmbus_read!(self.device, VOUT_MODE)?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }
}

impl Validate<Error> for Versal {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = &[0x00, 0x00, 0x93];
        pmbus_validate(device, CommandCode::MFR_ID, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Versal {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, versal::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl VoltageSensor<Error> for Versal {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, versal::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}
