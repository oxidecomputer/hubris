// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the TPS546B24A buck converter

use core::cell::Cell;

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, TempSensor, Validate,
    VoltageSensor,
};
use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Tps546B24A {
    device: I2cDevice,
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
            _ => panic!(),
        }
    }
}

impl Tps546B24A {
    pub fn new(device: &I2cDevice, _rail: u8) -> Self {
        Tps546B24A {
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

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<Error> for Tps546B24A {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = &[0x54, 0x49, 0x54, 0x6B, 0x24, 0x41];
        pmbus_validate(device, CommandCode::IC_DEVICE_ID, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Tps546B24A {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, tps546b24a::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Tps546B24A {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, tps546b24a::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl VoltageSensor<Error> for Tps546B24A {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, tps546b24a::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl crate::PmbusVpd for Tps546B24A {
    const HAS_MFR_DATE: bool = false;
    const HAS_MFR_LOCATION: bool = false;
    const HAS_MFR_SERIAL: bool = true;
    const HAS_IC_DEVICE_IDENTITY: bool = true;
}
