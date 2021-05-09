//! Driver for the ADM1272 hot-swap controller

use bitfield::bitfield;
use drv_i2c_api::*;
use drv_pmbus::*;
use num_traits::float::FloatCore;
use ringbuf::*;
use userlib::units::*;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
enum SampleAveraging {
    Disabled = 0b000,
    Average2 = 0b001,
    Average4 = 0b010,
    Average8 = 0b011,
    Average16 = 0b100,
    Average32 = 0b101,
    Average64 = 0b110,
    Average128 = 0b111,
}

impl From<u16> for SampleAveraging {
    fn from(value: u16) -> Self {
        SampleAveraging::from_u16(value).unwrap()
    }
}

impl From<SampleAveraging> for u16 {
    fn from(value: SampleAveraging) -> Self {
        value as u16
    }
}

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
    pub struct PowerMonitorConfiguration(u16);
    tsfilt, set_tsfilt: 15;
    simultaneous, set_simultaneous: 14;
    from into SampleAveraging, pwr_avg, set_pwr_avg: 13, 11;
    from into SampleAveraging, vi_avg, set_vi_avg: 10, 8;
    vrange_100v, set_vrange_100v: 5;
    pmon_mode_continuous, set_pmon_mode_continuous: 4;
    temp1_enable, set_temp1_enable: 3;
    vin_enable, set_vin_enable: 2;
    vout_enable, set_vout_enable: 1;
    irange_30mv, set_irange_30mv: 0;
}

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Register {
    ProgrammableRestart,
    PeakOutputCurrent,
    PeakInputVoltage,
    PeakOutputVoltage,
    PowerMonitorControl,
    PowerMonitorConfiguration,
    Alert1Configuration,
    Alert2Configuration,
    PeakTemperature,
    DeviceConfiguration,
    PowerCycle,
    PeakPower,
    ReadPower,
    ReadEnergy,
    HysteresisLowLevel,
    HysteresisHighLevel,
    HysteresisStatus,
    GPIOPinStatus,
    StartupCurrentLimit,
    PMBus(Command),
}

impl From<Register> for u8 {
    fn from(value: Register) -> Self {
        match value {
            Register::ProgrammableRestart => 0xcc,
            Register::PeakOutputCurrent => 0xd0,
            Register::PeakInputVoltage => 0xd1,
            Register::PeakOutputVoltage => 0xd2,
            Register::PowerMonitorControl => 0xd3,
            Register::PowerMonitorConfiguration => 0xd4,
            Register::Alert1Configuration => 0xd5,
            Register::Alert2Configuration => 0xd6,
            Register::PeakTemperature => 0xd7,
            Register::DeviceConfiguration => 0xd8,
            Register::PowerCycle => 0xd9,
            Register::PeakPower => 0xda,
            Register::ReadPower => 0xdb,
            Register::ReadEnergy => 0xdc,
            Register::HysteresisLowLevel => 0xf2,
            Register::HysteresisHighLevel => 0xf3,
            Register::HysteresisStatus => 0xf4,
            Register::GPIOPinStatus => 0xf5,
            Register::StartupCurrentLimit => 0xf6,
            Register::PMBus(cmd) => cmd as u8,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    BadRead16 { reg: Register, code: ResponseCode },
}

pub struct Adm1272 {
    device: I2cDevice,
    rsense: i32,
    voltage_coefficients: Option<Coefficients>,
    current_coefficients: Option<Coefficients>,
    power_coefficients: Option<Coefficients>,
}

impl core::fmt::Display for Adm1272 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adm1272: {}", &self.device)
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Read16(Register, u16),
    Config(PowerMonitorConfiguration),
    ReadError(Register, ResponseCode),
    Coefficients(Coefficients),
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
        }
    }

    fn read_reg16(&self, register: Register) -> Result<u16, Error> {
        let rval = self.device.read_reg::<u8, [u8; 2]>(u8::from(register));

        match rval {
            Ok(val) => {
                let v = ((val[1] as u16) << 8) | val[0] as u16;
                ringbuf_entry!(Trace::Read16(register, v));
                Ok(v)
            }

            Err(code) => {
                ringbuf_entry!(Trace::ReadError(register, code));
                Err(Error::BadRead16 {
                    reg: register,
                    code: code,
                })
            }
        }
    }

    fn load_coefficients(&mut self) -> Result<(), Error> {
        let config = PowerMonitorConfiguration(
            self.read_reg16(Register::PowerMonitorConfiguration)?,
        );

        self.voltage_coefficients = Some(if config.vrange_100v() {
            Coefficients {
                m: 4062,
                b: 0,
                R: -2,
            }
        } else {
            Coefficients {
                m: 6770,
                b: 0,
                R: -2,
            }
        });

        ringbuf_entry!(Trace::Coefficients(self.voltage_coefficients.unwrap()));

        self.current_coefficients = Some(if config.irange_30mv() {
            Coefficients {
                m: 663 * self.rsense,
                b: 20480,
                R: -1,
            }
        } else {
            Coefficients {
                m: 1326 * self.rsense,
                b: 20480,
                R: -1,
            }
        });

        ringbuf_entry!(Trace::Coefficients(self.current_coefficients.unwrap()));

        let power = match (config.irange_30mv(), config.vrange_100v()) {
            (false, false) => Coefficients {
                m: 3512 * self.rsense,
                b: 0,
                R: -2,
            },
            (false, true) => Coefficients {
                m: 21071 * self.rsense,
                b: 0,
                R: -3,
            },
            (true, false) => Coefficients {
                m: 17561 * self.rsense,
                b: 0,
                R: -3,
            },
            (true, true) => Coefficients {
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

    pub fn read_manufacturer(
        &self,
        buf: &mut [u8],
    ) -> Result<(), ResponseCode> {
        self.device.read_block(Command::ManufacturerID as u8, buf)
    }

    pub fn read_model(&self, buf: &mut [u8]) -> Result<(), ResponseCode> {
        self.device
            .read_block(Command::ManufacturerModel as u8, buf)
    }

    pub fn read_vin(&mut self) -> Result<Volts, Error> {
        let vin = self.read_reg16(Register::PMBus(Command::ReadVIn))?;
        Ok(Volts(Direct(vin, self.voltage_coefficients()?).to_real()))
    }

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = self.read_reg16(Register::PMBus(Command::ReadVOut))?;
        Ok(Volts(Direct(vout, self.voltage_coefficients()?).to_real()))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = self.read_reg16(Register::PMBus(Command::ReadIOut))?;
        Ok(Amperes(
            Direct(iout, self.current_coefficients()?).to_real(),
        ))
    }
}
