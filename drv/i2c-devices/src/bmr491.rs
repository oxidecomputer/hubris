// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the BMR491 IBC

use core::cell::Cell;

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, TempSensor, Validate,
    VoltageSensor,
};
use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Bmr491 {
    device: I2cDevice,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

#[derive(Debug)]
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

impl Bmr491 {
    pub fn new(device: &I2cDevice, _rail: u8) -> Self {
        Bmr491 {
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

    pub fn set_vout(&self, v: u16) -> Result<(), Error> {
        let mut vout = VOUT_COMMAND::CommandData(0);
        let value = Volts(v as f32);
        vout.set(self.read_mode()?, pmbus::units::Volts(value.0))?;
        pmbus_write!(self.device, VOUT_COMMAND, vout)
    }

    pub fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, bmr491::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<Error> for Bmr491 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"Flex";
        pmbus_validate(device, CommandCode::MFR_ID, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Bmr491 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, bmr491::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Bmr491 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, bmr491::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl VoltageSensor<Error> for Bmr491 {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, bmr491::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}
