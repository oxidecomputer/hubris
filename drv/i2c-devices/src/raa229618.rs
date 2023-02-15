// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::Cell;

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, TempSensor, Validate,
    VoltageSensor,
};
use drv_i2c_api::*;
use pmbus::commands::raa229618::*;
use pmbus::commands::CommandCode;
use pmbus::*;
use userlib::units::*;

pub struct Raa229618 {
    device: I2cDevice,
    rail: u8,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

impl core::fmt::Display for Raa229618 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "raa229618: {}", &self.device)
    }
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

impl Raa229618 {
    pub fn new(device: &I2cDevice, rail: u8) -> Self {
        Raa229618 {
            device: *device,
            rail,
            mode: Cell::new(None),
        }
    }

    pub fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode.get() {
            None => {
                let mode = pmbus_read!(self.device, commands::VOUT_MODE)?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }

    fn set_rail(&self) -> Result<(), Error> {
        let page = PAGE::CommandData(self.rail);
        pmbus_write!(self.device, PAGE, page)
    }

    pub fn turn_off(&mut self) -> Result<(), Error> {
        self.set_rail()?;
        let mut operation = pmbus_read!(self.device, OPERATION)?;
        operation.set_on_off_state(OPERATION::OnOffState::Off);
        pmbus_write!(self.device, OPERATION, operation)
    }

    pub fn turn_on(&mut self) -> Result<(), Error> {
        self.set_rail()?;
        let mut operation = pmbus_read!(self.device, OPERATION)?;
        operation.set_on_off_state(OPERATION::OnOffState::On);
        pmbus_write!(self.device, OPERATION, operation)
    }

    pub fn set_vout(&mut self, value: Volts) -> Result<(), Error> {
        if value > Volts(3.050) {
            Err(Error::InvalidData {
                err: pmbus::Error::ValueOutOfRange,
            })
        } else {
            self.set_rail()?;
            let mut vout = VOUT_COMMAND::CommandData(0);
            vout.set(self.read_mode()?, pmbus::units::Volts(value.0))?;
            pmbus_write!(self.device, VOUT_COMMAND, vout)
        }
    }
}

impl Validate<Error> for Raa229618 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = &[0x00, 0x99, 0xd2, 0x49];
        pmbus_validate(device, CommandCode::IC_DEVICE_ID, expected)
            .map_err(Into::into)
    }
}

impl VoltageSensor<Error> for Raa229618 {
    fn read_vout(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vout = pmbus_read!(self.device, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl TempSensor<Error> for Raa229618 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        self.set_rail()?;
        let temp = pmbus_read!(self.device, READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Raa229618 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iout = pmbus_read!(self.device, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
