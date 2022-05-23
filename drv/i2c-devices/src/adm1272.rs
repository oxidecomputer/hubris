// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the ADM1272 hot-swap controller

use core::cell::Cell;

use crate::{CurrentSensor, TempSensor, Validate, VoltageSensor};
use drv_i2c_api::*;
use num_traits::float::FloatCore;
use pmbus::commands::*;
use ringbuf::*;
use userlib::units::*;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
    InvalidData { err: pmbus::Error },
    InvalidConfig,
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

#[derive(Copy, Clone)]
#[allow(dead_code)]
struct Coefficients {
    voltage: pmbus::Coefficients,
    current: pmbus::Coefficients,
    power: pmbus::Coefficients,
}

pub struct Adm1272 {
    /// Underlying I2C device
    device: I2cDevice,
    /// Value of the rsense resistor, in milliohms
    rsense: i32,
    /// Our (cached) coefficients
    coefficients: Cell<Option<Coefficients>>,
    /// Our (cached) configuration
    config: Cell<Option<adm1272::PMON_CONFIG::CommandData>>,
}

impl core::fmt::Display for Adm1272 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adm1272: {}", &self.device)
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Coefficients(pmbus::Coefficients),
    Config(adm1272::PMON_CONFIG::CommandData),
    WriteConfig(adm1272::PMON_CONFIG::CommandData),
    None,
}

ringbuf!(Trace, 32, Trace::None);

impl Adm1272 {
    pub fn new(device: &I2cDevice, rsense: Ohms) -> Self {
        Self {
            device: *device,
            rsense: (rsense.0 * 1000.0).round() as i32,
            coefficients: Cell::new(None),
            config: Cell::new(None),
        }
    }

    fn read_config(&self) -> Result<adm1272::PMON_CONFIG::CommandData, Error> {
        if let Some(ref config) = self.config.get() {
            return Ok(*config);
        }

        let config = pmbus_read!(self.device, adm1272::PMON_CONFIG)?;
        ringbuf_entry!(Trace::Config(config));
        self.config.set(Some(config));

        Ok(config)
    }

    fn write_config(
        &self,
        config: adm1272::PMON_CONFIG::CommandData,
    ) -> Result<(), Error> {
        ringbuf_entry!(Trace::WriteConfig(config));
        let out = pmbus_write!(self.device, adm1272::PMON_CONFIG, config);
        if out.is_err() {
            // If the write fails, invalidate the cache, since we don't
            // know exactly what state the remote system ended up in.
            self.config.set(None);
        }
        out
    }

    //
    // Unlike many/most PMBus devices that have one set of coefficients, the
    // coefficients for the ADM1272 depends on the mode of the device.  We
    // therefore determine these dynamically -- but cache the results.
    //
    fn load_coefficients(&self) -> Result<Coefficients, Error> {
        use adm1272::PMON_CONFIG::*;

        if let Some(coefficients) = self.coefficients.get() {
            return Ok(coefficients);
        }

        let config = self.read_config()?;

        let vrange = config.get_v_range().ok_or(Error::InvalidConfig)?;
        let irange = config.get_i_range().ok_or(Error::InvalidConfig)?;

        //
        // From Table 10 (columns 1 and 2) of the ADM1272 datasheet.
        //
        let voltage = match vrange {
            VRange::Range100V => pmbus::Coefficients {
                m: 4062,
                b: 0,
                R: -2,
            },
            VRange::Range60V => pmbus::Coefficients {
                m: 6770,
                b: 0,
                R: -2,
            },
        };

        ringbuf_entry!(Trace::Coefficients(voltage));

        //
        // From Table 10 (columns 3 and 4) of the ADM1272 datasheet.
        //
        let current = match irange {
            IRange::Range30mV => pmbus::Coefficients {
                m: 663 * self.rsense,
                b: 20480,
                R: -1,
            },
            IRange::Range15mV => pmbus::Coefficients {
                m: 1326 * self.rsense,
                b: 20480,
                R: -1,
            },
        };

        ringbuf_entry!(Trace::Coefficients(current));

        //
        // From Table 10 (columns 5 through 8) of the ADM1272 datasheet.
        //
        let power = match (irange, vrange) {
            (IRange::Range15mV, VRange::Range60V) => pmbus::Coefficients {
                m: 3512 * self.rsense,
                b: 0,
                R: -2,
            },
            (IRange::Range15mV, VRange::Range100V) => pmbus::Coefficients {
                m: 21071 * self.rsense,
                b: 0,
                R: -3,
            },
            (IRange::Range30mV, VRange::Range60V) => pmbus::Coefficients {
                m: 17561 * self.rsense,
                b: 0,
                R: -3,
            },
            (IRange::Range30mV, VRange::Range100V) => pmbus::Coefficients {
                m: 10535 * self.rsense,
                b: 0,
                R: -3,
            },
        };

        ringbuf_entry!(Trace::Coefficients(power));

        self.coefficients.set(Some(Coefficients {
            voltage,
            current,
            power,
        }));
        Ok(self.coefficients.get().unwrap())
    }

    fn enable_vin_sampling(&self) -> Result<(), Error> {
        use adm1272::PMON_CONFIG::*;
        let mut config = self.read_config()?;

        match config.get_v_in_enable() {
            None => Err(Error::InvalidConfig),
            Some(VInEnable::Disabled) => {
                config.set_v_in_enable(VInEnable::Enabled);
                self.write_config(config)
            }
            _ => Ok(()),
        }
    }

    fn enable_vout_sampling(&self) -> Result<(), Error> {
        use adm1272::PMON_CONFIG::*;
        let mut config = self.read_config()?;

        match config.get_v_out_enable() {
            None => Err(Error::InvalidConfig),
            Some(VOutEnable::Disabled) => {
                config.set_v_out_enable(VOutEnable::Enabled);
                self.write_config(config)
            }
            _ => Ok(()),
        }
    }

    fn enable_temp1_sampling(&self) -> Result<(), Error> {
        use adm1272::PMON_CONFIG::*;
        let mut config = self.read_config()?;

        match config.get_temp_1_enable() {
            None => Err(Error::InvalidConfig),
            Some(Temp1Enable::Disabled) => {
                config.set_temp_1_enable(Temp1Enable::Enabled);
                self.write_config(config)
            }
            _ => Ok(()),
        }
    }

    pub fn read_vin(&self) -> Result<Volts, Error> {
        self.enable_vin_sampling()?;
        let vin = pmbus_read!(self.device, adm1272::READ_VIN)?;
        Ok(Volts(vin.get(&self.load_coefficients()?.voltage)?.0))
    }

    pub fn peak_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, adm1272::PEAK_IOUT)?;
        Ok(Amperes(iout.get(&self.load_coefficients()?.current)?.0))
    }
}

impl Validate<Error> for Adm1272 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let id = [0x41, 0x44, 0x4d, 0x31, 0x32, 0x37, 0x32, 0x2d, 0x32, 0x41];
        pmbus_validate!(device, MFR_MODEL, id)
    }
}

impl TempSensor<Error> for Adm1272 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        self.enable_temp1_sampling()?;
        let temp = pmbus_read!(self.device, adm1272::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Adm1272 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, adm1272::READ_IOUT)?;
        Ok(Amperes(iout.get(&self.load_coefficients()?.current)?.0))
    }
}

impl VoltageSensor<Error> for Adm1272 {
    fn read_vout(&self) -> Result<Volts, Error> {
        self.enable_vout_sampling()?;
        let vout = pmbus_read!(self.device, adm1272::READ_VOUT)?;
        Ok(Volts(vout.get(&self.load_coefficients()?.voltage)?.0))
    }
}
