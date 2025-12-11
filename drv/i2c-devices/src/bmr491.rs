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

    /// Applies a firmware-configuration workaround for the component tolerance
    /// issue described in (unfortunately confidential) Flex document
    /// RMA2402311, where over-eager input undervoltage sensing can result in
    /// output glitches.
    ///
    /// The theory of operation of the mitigation is as follows:
    ///
    /// - An input filter in the R1C revision was built incorrectly and allows
    ///   brief negative transients to reach the controller chip.
    ///
    /// - To stop the controller chip from reacting to these transients, Flex
    ///   suggests disabling automatic response to input undervoltage events.
    ///
    /// - To retain some degree of brownout protection, they recommend instead
    ///   enabling the _output_ undervoltage monitor.
    ///
    /// This function will check to see if the mitigation is already applied, or
    /// if the device reports an unaffected revision. In either case, it will
    /// leave the settings untouched.
    pub fn apply_mitigation_for_rma2402311(&self) -> Result<(), Error> {
        use pmbus::commands::bmr491::{CommandCode, VIN_OFF, VOUT_COMMAND, MAX_DUTY, VOUT_UV_FAULT_LIMIT};
        let mut rev = [0u8; 12];
        self.device.read_block(CommandCode::MFR_REVISION as u8, &mut rev)
            .map_err(|code| Error::BadRead { cmd: CommandCode::MFR_REVISION as u8, code })?;

        // Currently, the defect is known to exist in R1C parts, and may have
        // been present in earlier parts (it's not clear that we have any). It
        // is supposed to be fixed in R1D, and in the event that an R1E is
        // released, it will _probably_ remain fixed.
        //
        // But since we don't know that for sure, currently we're treating only
        // the R1D revision as fixed. Applying the mitigation to fixed future
        // IBCs should not be destructive, so we can fix the firmware when that
        // day comes.
        if rev.starts_with(b"R1D ") {
            // Assume the mitigation is unnecessary.
            return Ok(());
        }

        // Read out the affected registers to see if we've already applied the
        // mitigation.
        let current_vin_off = pmbus_read!(self.device, VIN_OFF)?;
        let current_vout_command = pmbus_read!(self.device, VOUT_COMMAND)?;
        let current_max_duty = pmbus_read!(self.device, MAX_DUTY)?;
        let current_vout_uv_fault_limit = pmbus_read!(self.device, VOUT_UV_FAULT_LIMIT)?;
        let current_vout_uv_fault_response = pmbus_read!(self.device, VOUT_UV_FAULT_RESPONSE)?;

        if current_vin_off.0 == 0
            && current_vout_command.0 == 0x0060
            && current_max_duty.0 == 0xF8EA
            && current_vout_uv_fault_limit.0 == 0x0058
            && current_vout_uv_fault_response.0 == 0x80
        {
            // The device configuration already reflects the mitigation.
            return Ok(());
        }
        

        // Override the VIN_OFF threshold to 0V, so that the IBC's internal
        // controller treats VIN as "always above threshold."
        pmbus_write!(self.device, VIN_OFF, VIN_OFF::CommandData(0))?;
        // Command the power supply to produce 12 V (each LSB in this register
        // is 1/8 V, so 12 * 8 = 96 = 0x60).
        pmbus_write!(self.device, VOUT_COMMAND, VOUT_COMMAND::CommandData(0x0060))?;
        // Override the max duty cycle. The rationale for this is not totally
        // clear, but Flex says to do it.
        pmbus_write!(self.device, MAX_DUTY, MAX_DUTY::CommandData(0xF8EA))?;
        // Adjust the VOUT fault to detect undervoltage on the 12V rail.
        pmbus_write!(self.device, VOUT_UV_FAULT_LIMIT, VOUT_UV_FAULT_LIMIT::CommandData(0x0058))?;
        // And configure it to shut off the IBC without retry on fault. (The
        // retry options are all wrong for our use case, they can cause it to
        // power itself on and off repeatedly before stopping, or just stop; we
        // choose the latter.)
        pmbus_write!(self.device, VOUT_UV_FAULT_RESPONSE, VOUT_UV_FAULT_RESPONSE::CommandData(0x80))?;

        pmbus_write!(self.device, STORE_USER_ALL)?;
        Ok(())
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
