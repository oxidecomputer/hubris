//! Driver for the ADM1272 hot-swap controller

use drv_i2c_api::*;
use num_traits::float::FloatCore;
use pmbus::commands::*;
use pmbus::*;
use ringbuf::*;
use userlib::units::*;

#[derive(Debug)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    InvalidData { err: pmbus::Error },
    InvalidConfig,
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err: err }
    }
}

pub struct Adm1272 {
    device: I2cDevice,
    rsense: i32,
    voltage_coefficients: Option<Coefficients>,
    current_coefficients: Option<Coefficients>,
    power_coefficients: Option<Coefficients>,
    config: Option<adm1272::PMON_CONFIG::CommandData>,
}

impl core::fmt::Display for Adm1272 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adm1272: {}", &self.device)
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Coefficients(Coefficients),
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
            voltage_coefficients: None,
            current_coefficients: None,
            power_coefficients: None,
            config: None,
        }
    }

    fn read_config(
        &mut self,
    ) -> Result<adm1272::PMON_CONFIG::CommandData, Error> {
        if let Some(ref config) = self.config {
            return Ok(*config);
        }

        let config = pmbus_read!(self.device, adm1272::PMON_CONFIG)?;
        ringbuf_entry!(Trace::Config(config));
        self.config = Some(config);

        Ok(config)
    }

    fn write_config(
        &mut self,
        config: adm1272::PMON_CONFIG::CommandData,
    ) -> Result<(), Error> {
        ringbuf_entry!(Trace::WriteConfig(config));
        pmbus_write!(self.device, adm1272::PMON_CONFIG, config)
    }

    fn load_coefficients(&mut self) -> Result<(), Error> {
        use adm1272::PMON_CONFIG::*;

        let config = self.read_config()?;

        let vrange = match config.get_v_range() {
            Some(vrange) => vrange,
            None => return Err(Error::InvalidConfig),
        };

        let irange = match config.get_i_range() {
            Some(irange) => irange,
            None => return Err(Error::InvalidConfig),
        };

        self.voltage_coefficients = Some(match vrange {
            VRange::Range100V => Coefficients {
                m: 4062,
                b: 0,
                R: -2,
            },
            VRange::Range60V => Coefficients {
                m: 6770,
                b: 0,
                R: -2,
            },
        });

        ringbuf_entry!(Trace::Coefficients(self.voltage_coefficients.unwrap()));

        self.current_coefficients = Some(match irange {
            IRange::Range30mV => Coefficients {
                m: 663 * self.rsense,
                b: 20480,
                R: -1,
            },
            IRange::Range15mV => Coefficients {
                m: 1326 * self.rsense,
                b: 20480,
                R: -1,
            },
        });

        ringbuf_entry!(Trace::Coefficients(self.current_coefficients.unwrap()));

        let power = match (irange, vrange) {
            (IRange::Range15mV, VRange::Range60V) => Coefficients {
                m: 3512 * self.rsense,
                b: 0,
                R: -2,
            },
            (IRange::Range15mV, VRange::Range100V) => Coefficients {
                m: 21071 * self.rsense,
                b: 0,
                R: -3,
            },
            (IRange::Range30mV, VRange::Range60V) => Coefficients {
                m: 17561 * self.rsense,
                b: 0,
                R: -3,
            },
            (IRange::Range30mV, VRange::Range100V) => Coefficients {
                m: 10535 * self.rsense,
                b: 0,
                R: -3,
            },
        };

        self.power_coefficients = Some(power);
        ringbuf_entry!(Trace::Coefficients(self.power_coefficients.unwrap()));

        Ok(())
    }

    fn voltage_coefficients(&mut self) -> Result<Coefficients, Error> {
        if let Some(ref coefficients) = self.voltage_coefficients {
            return Ok(*coefficients);
        }

        self.load_coefficients()?;
        Ok(self.voltage_coefficients.unwrap())
    }

    fn current_coefficients(&mut self) -> Result<Coefficients, Error> {
        if let Some(ref coefficients) = self.current_coefficients {
            return Ok(*coefficients);
        }

        self.load_coefficients()?;
        Ok(self.current_coefficients.unwrap())
    }

    fn enable_vin(&mut self) -> Result<(), Error> {
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

    fn enable_vout(&mut self) -> Result<(), Error> {
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

    pub fn read_vin(&mut self) -> Result<Volts, Error> {
        self.enable_vin()?;
        let vin = pmbus_read!(self.device, adm1272::READ_VIN)?;
        Ok(Volts(vin.get(&self.voltage_coefficients()?)?.0))
    }

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        self.enable_vout()?;
        let vout = pmbus_read!(self.device, adm1272::READ_VOUT)?;
        Ok(Volts(vout.get(&self.voltage_coefficients()?)?.0))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, adm1272::READ_IOUT)?;
        Ok(Amperes(iout.get(&self.current_coefficients()?)?.0))
    }

    pub fn peak_iout(&mut self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, adm1272::PEAK_IOUT)?;
        Ok(Amperes(iout.get(&self.current_coefficients()?)?.0))
    }
}
