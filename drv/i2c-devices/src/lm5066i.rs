// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LM5066I hot-swap controller
//!
//! This is very similar to the LM5066, but has different dynamic coefficients
//! and serial name (for validation).

use core::cell::Cell;

use crate::{
    pmbus_validate, CurrentSensor, TempSensor, Validate, VoltageSensor,
};
use drv_i2c_api::*;
use num_traits::float::FloatCore;
use pmbus::commands::*;
use ringbuf::*;
use userlib::units::*;

use crate::lm5066::Coefficients;
pub use crate::lm5066::{CurrentLimitStrap, Error};

#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Trace {
    None,
    CurrentCoefficients(pmbus::Coefficients),
    PowerCoefficients(pmbus::Coefficients),
    DeviceSetup(lm5066i::DEVICE_SETUP::CommandData),
}

pub struct Lm5066I {
    /// Underlying I2C device
    device: I2cDevice,
    /// Value of the rsense resistor, in milliohms
    rsense: i32,
    /// Sense of current limit pin
    cl: CurrentLimitStrap,
    /// Our (cached) coefficients
    coefficients: Cell<Option<Coefficients>>,
    /// Our (cached) device setup
    device_setup: Cell<Option<lm5066i::DEVICE_SETUP::CommandData>>,
}

impl core::fmt::Display for Lm5066I {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "lm5066i: {}", &self.device)
    }
}

ringbuf!(Trace, 8, Trace::None);

impl Lm5066I {
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
    ) -> Result<lm5066i::DEVICE_SETUP::CommandData, Error> {
        if let Some(ref device_setup) = self.device_setup.get() {
            return Ok(*device_setup);
        }

        let device_setup = pmbus_read!(self.device, lm5066i::DEVICE_SETUP)?;
        ringbuf_entry!(Trace::DeviceSetup(device_setup));
        self.device_setup.set(Some(device_setup));

        Ok(device_setup)
    }

    pub fn clear_faults(&self) -> Result<(), Error> {
        pmbus_write!(self.device, CLEAR_FAULTS)
    }

    pub fn enable_averaging(&self, log2_count: u8) -> Result<(), Error> {
        if log2_count > 0xC {
            return Err(Error::InvalidDeviceSetup);
        }
        let count = lm5066i::SAMPLES_FOR_AVG::CommandData(log2_count);
        pmbus_write!(self.device, lm5066i::SAMPLES_FOR_AVG, count)
    }

    ///
    /// The coefficients for the LM5066I depend on the value of the current
    /// sense resistor and the sense of the current limit (CL) strap.
    /// Unfortunately, DEVICE_SETUP will not tell us the physical sense of
    /// this strap; we rely on this information to be provided when the
    /// device is initialized.
    ///
    fn load_coefficients(&self) -> Result<Coefficients, Error> {
        use lm5066i::DEVICE_SETUP::*;

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
        // From Table 48 of the LM5066I datasheet.  Note that the datasheet has
        // an admonishment about adjusting R to keep m to within a signed
        // 16-bit quantity (that is, no larger than 32767), but we actually
        // treat m as a 32-bit quantity so there is no need to clamp it here.
        // (At the maximum of 200 mΩ, m is well within a 32-bit quantity.)
        //
        let current = match cl {
            CurrentLimitStrap::GND => pmbus::Coefficients {
                m: 7645 * self.rsense,
                b: 100,
                R: -2,
            },
            CurrentLimitStrap::VDD => pmbus::Coefficients {
                m: 15076 * self.rsense,
                b: -504,
                R: -2,
            },
        };

        ringbuf_entry!(Trace::CurrentCoefficients(current));

        let power = match cl {
            CurrentLimitStrap::GND => pmbus::Coefficients {
                m: 861 * self.rsense,
                b: -965,
                R: -3,
            },
            CurrentLimitStrap::VDD => pmbus::Coefficients {
                m: 1701 * self.rsense,
                b: -4000,
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

impl Validate<Error> for Lm5066I {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"LM5066I\0";
        pmbus_validate(device, CommandCode::MFR_MODEL, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Lm5066I {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, lm5066i::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Lm5066I {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, lm5066i::READ_AVG_IIN)?;
        Ok(Amperes(iout.get(&self.load_coefficients()?.current)?.0))
    }
}

impl VoltageSensor<Error> for Lm5066I {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, lm5066i::READ_AVG_VOUT)?;
        Ok(Volts(vout.get()?.0))
    }
}
