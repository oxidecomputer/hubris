// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the TPS546B24A buck converter

use crate::{CurrentSensor, TempSensor, Validate, VoltageSensor};
use drv_i2c_api::*;
use pmbus::commands::*;
use userlib::units::*;

pub struct Tps546B24A {
    device: I2cDevice,
    mode: Option<pmbus::VOutModeCommandData>,
}

#[derive(Debug)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
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

impl Tps546B24A {
    pub fn new(device: &I2cDevice, _rail: u8) -> Self {
        Tps546B24A {
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
}

impl Validate<Error> for Tps546B24A {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = [0x54, 0x49, 0x54, 0x6B, 0x24, 0x41];
        pmbus_validate!(device, IC_DEVICE_ID, expected)
    }
}

impl TempSensor<Error> for Tps546B24A {
    fn read_temperature(&mut self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, tps546b24a::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Tps546B24A {
    fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, tps546b24a::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl VoltageSensor<Error> for Tps546B24A {
    fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, tps546b24a::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}
