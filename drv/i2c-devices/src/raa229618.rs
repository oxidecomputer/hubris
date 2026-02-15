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

//
// This is a special rail value that is issued as a PAGE command to enable
// reading phase current via PHASE + PHASE_CURRENT
//
const PHASE_RAIL: u8 = 0x80;

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

#[derive(Copy, Clone, Debug, PartialEq)]
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
                let mode = pmbus_rail_read!(
                    self.device,
                    self.rail,
                    commands::VOUT_MODE
                )?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }

    pub fn turn_off(&mut self) -> Result<(), Error> {
        let mut op = pmbus_rail_read!(self.device, self.rail, OPERATION)?;
        op.set_on_off_state(OPERATION::OnOffState::Off);
        pmbus_rail_write!(self.device, self.rail, OPERATION, op)
    }

    pub fn turn_on(&mut self) -> Result<(), Error> {
        let mut op = pmbus_rail_read!(self.device, self.rail, OPERATION)?;
        op.set_on_off_state(OPERATION::OnOffState::On);
        pmbus_rail_write!(self.device, self.rail, OPERATION, op)
    }

    pub fn set_vout(&mut self, value: Volts) -> Result<(), Error> {
        if value > Volts(3.050) {
            Err(Error::InvalidData {
                err: pmbus::Error::ValueOutOfRange,
            })
        } else {
            let mut vout = VOUT_COMMAND::CommandData(0);
            vout.set(self.read_mode()?, pmbus::units::Volts(value.0))?;
            pmbus_rail_write!(self.device, self.rail, VOUT_COMMAND, vout)
        }
    }

    pub fn clear_faults(&self) -> Result<(), Error> {
        pmbus_rail_write!(self.device, self.rail, CLEAR_FAULTS)
    }

    pub fn set_vin_uv_warn_limit(&self, value: Volts) -> Result<(), Error> {
        let mut vin = VIN_UV_WARN_LIMIT::CommandData(0);
        vin.set(pmbus::units::Volts(value.0))?;
        pmbus_rail_write!(self.device, self.rail, VIN_UV_WARN_LIMIT, vin)
    }

    pub fn read_vin(&self) -> Result<Volts, Error> {
        let vin = pmbus_rail_read!(self.device, self.rail, READ_VIN)?;
        Ok(Volts(vin.get()?.0))
    }

    pub fn read_phase_current(&self, phase: Phase) -> Result<Amperes, Error> {
        let iout = pmbus_rail_phase_read!(
            self.device,
            PHASE_RAIL,
            phase.0,
            PHASE_CURRENT
        )?;
        Ok(Amperes(iout.get()?.0))
    }

    pub fn status_word(&self) -> Result<STATUS_WORD::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_WORD)
    }

    pub fn status_iout(&self) -> Result<STATUS_IOUT::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_IOUT)
    }

    pub fn status_vout(&self) -> Result<STATUS_VOUT::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_VOUT)
    }

    pub fn status_input(&self) -> Result<STATUS_INPUT::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_INPUT)
    }

    pub fn status_cml(&self) -> Result<STATUS_CML::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_CML)
    }

    pub fn status_temperature(
        &self,
    ) -> Result<STATUS_TEMPERATURE::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_TEMPERATURE)
    }

    pub fn status_mfr_specific(
        &self,
    ) -> Result<STATUS_MFR_SPECIFIC::CommandData, Error> {
        pmbus_rail_read!(self.device, self.rail, STATUS_MFR_SPECIFIC)
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }

    pub fn rail(&self) -> u8 {
        self.rail
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
        let vout = pmbus_rail_read!(self.device, self.rail, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl TempSensor<Error> for Raa229618 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let t = pmbus_rail_read!(self.device, self.rail, READ_TEMPERATURE_1)?;
        Ok(Celsius(t.get()?.0))
    }
}

impl CurrentSensor<Error> for Raa229618 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_rail_read!(self.device, self.rail, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}
