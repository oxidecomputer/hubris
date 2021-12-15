// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_api::*;
use pmbus::commands::raa229618::*;
use pmbus::*;
use userlib::units::*;

pub struct Raa229618 {
    device: I2cDevice,
    rail: u8,
    mode: Option<pmbus::VOutModeCommandData>,
}

impl core::fmt::Display for Raa229618 {
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

impl Raa229618 {
    pub fn new(device: &I2cDevice, rail: u8) -> Self {
        Raa229618 {
            device: *device,
            rail: rail,
            mode: None,
        }
    }

    fn read_mode(&mut self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode {
            None => {
                let mode = pmbus_read!(self.device, commands::VOUT_MODE)?;
                self.mode = Some(mode);
                mode
            }
            Some(mode) => mode,
        })
    }

    fn set_rail(&mut self) -> Result<(), Error> {
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

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vout = pmbus_read!(self.device, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iout = pmbus_read!(self.device, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
