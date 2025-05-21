// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LM5066 hot-swap controller

use core::cell::Cell;

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, TempSensor, Validate,
    VoltageSensor,
};
use drv_i2c_api::*;
use num_traits::float::FloatCore;
use pmbus::commands::*;
use ringbuf::*;
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

    /// Device setup is invalid
    InvalidDeviceSetup,
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
            Error::BadData { .. }
            | Error::InvalidData { .. }
            | Error::InvalidDeviceSetup => ResponseCode::BadDeviceState,
        }
    }
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
struct Coefficients {
    current: pmbus::Coefficients,
    power: pmbus::Coefficients,
}

#[derive(Copy, Clone)]
pub enum CurrentLimitStrap {
    /// CL pin is strapped to VDD, denoting the Low setting
    VDD,
    /// CL pin is strapped to GND (or left floating), denoting the high setting
    GND,
}

pub struct Lm5066 {
    /// Underlying I2C device
    device: I2cDevice,
    /// Value of the rsense resistor, in milliohms
    rsense: i32,
    /// Sense of current limit pin
    cl: CurrentLimitStrap,
    /// Our (cached) coefficients
    coefficients: Cell<Option<Coefficients>>,
    /// Our (cached) device setup
    device_setup: Cell<Option<lm5066::DEVICE_SETUP::CommandData>>,
}

impl core::fmt::Display for Lm5066 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "lm5066: {}", &self.device)
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    CurrentCoefficients(pmbus::Coefficients),
    PowerCoefficients(pmbus::Coefficients),
    DeviceSetup(lm5066::DEVICE_SETUP::CommandData),
    None,
}

ringbuf!(Trace, 8, Trace::None);

impl Lm5066 {
    pub fn new(
        device: &I2cDevice,
        rsense: Ohms,
        cl: CurrentLimitStrap,
    ) -> Self {
        Self {
            device: *device,
            rsense: (rsense.0 * 1000.0).round() as i32,
            cl,
            coefficients: Cell::new(None),
            device_setup: Cell::new(None),
        }
    }

    fn read_device_setup(
        &self,
    ) -> Result<lm5066::DEVICE_SETUP::CommandData, Error> {
        if let Some(ref device_setup) = self.device_setup.get() {
            return Ok(*device_setup);
        }

        let device_setup = pmbus_read!(self.device, lm5066::DEVICE_SETUP)?;
        ringbuf_entry!(Trace::DeviceSetup(device_setup));
        self.device_setup.set(Some(device_setup));

        Ok(device_setup)
    }

    pub fn clear_faults(&self) -> Result<(), Error> {
        pmbus_write!(self.device, CLEAR_FAULTS)
    }

    ///
    /// The coefficients for the LM5066 depend on the value of the current
    /// sense resistor and the sense of the current limit (CL) strap.
    /// Unfortunately, DEVICE_SETUP will not tell us the physical sense of
    /// this strap; we rely on this information to be provided when the
    /// device is initialized.
    ///
    fn load_coefficients(&self) -> Result<Coefficients, Error> {
        use lm5066::DEVICE_SETUP::*;

        if let Some(coefficients) = self.coefficients.get() {
            return Ok(coefficients);
        }

        let device_setup = self.read_device_setup()?;

        let setting = device_setup
            .get_current_setting()
            .ok_or(Error::InvalidDeviceSetup)?;

        let config = device_setup
            .get_current_config()
            .ok_or(Error::InvalidDeviceSetup)?;

        let cl = match config {
            CurrentConfig::Pin => self.cl,
            CurrentConfig::SMBus => match setting {
                CurrentSetting::Low => CurrentLimitStrap::VDD,
                CurrentSetting::High => CurrentLimitStrap::GND,
            },
        };

        //
        // From Table 43 of the LM5066 datasheet.  Note that the datasheet has
        // an admonishment about adjusting R to keep m to within a signed
        // 16-bit quantity (that is, no larger than 32767), but we actually
        // treat m as a 32-bit quantity so there is no need to clamp it here.
        // (At the maximum of 200 mΩ, m is well within a 32-bit quantity.)
        //
        let current = match cl {
            CurrentLimitStrap::GND => pmbus::Coefficients {
                m: 5405 * self.rsense,
                b: -600,
                R: -2,
            },
            CurrentLimitStrap::VDD => pmbus::Coefficients {
                m: 10753 * self.rsense,
                b: -1200,
                R: -2,
            },
        };

        ringbuf_entry!(Trace::CurrentCoefficients(current));

        let power = match cl {
            CurrentLimitStrap::GND => pmbus::Coefficients {
                m: 605 * self.rsense,
                b: -8000,
                R: -3,
            },
            CurrentLimitStrap::VDD => pmbus::Coefficients {
                m: 1204 * self.rsense,
                b: -6000,
                R: -3,
            },
        };

        ringbuf_entry!(Trace::PowerCoefficients(power));

        self.coefficients.set(Some(Coefficients { current, power }));
        Ok(self.coefficients.get().unwrap())
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<Error> for Lm5066 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"LM5066I\0";
        pmbus_validate(device, CommandCode::MFR_MODEL, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Lm5066 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, lm5066::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Lm5066 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, lm5066::MFR_READ_IIN)?;
        Ok(Amperes(iout.get(&self.load_coefficients()?.current)?.0))
    }
}

impl VoltageSensor<Error> for Lm5066 {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, lm5066::READ_VOUT)?;
        Ok(Volts(vout.get()?.0))
    }
}
