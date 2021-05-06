//! Driver for the ADM1272 hot-swap controller

use bitfield::bitfield;
use drv_i2c_api::*;
use drv_pmbus::*;
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
    pub struct PowerMonitorConfiguration(u16);
    tsfilt, set_tsfilt: 15;
    simultaneous, set_simultaneous: 14;
    from into SampleAveraging, pwr_avg, set_pwr_avg: 13, 11;
    from into SampleAveraging, vi_avg, set_vi_avg: 10, 8;
    vrange, set_vrange: 5;
    pmon_mode_continuous, set_pmon_mode_continuous: 4;
    temp1_enable, set_temp1_enable: 3;
    vin_enable, set_vin_enable: 2;
    vout_enable, set_vout_enable: 1;
    irange, set_irange: 0;
}

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub enum Register {
    PowerMonitorControl,
    PowerMonitorConfiguration,
    PMBus(Command),
}

impl From<Register> for u8 {
    fn from(value: Register) -> Self {
        match value {
            Register::PowerMonitorControl => 0xd3,
            Register::PowerMonitorConfiguration => 0xd4,
            Register::PMBus(cmd) => cmd as u8,
        }
    }
}

pub enum Error {
    BadRead16 { reg: Register, code: ResponseCode },
}

pub struct Adm1272 {
    device: I2cDevice,
}

impl core::fmt::Display for Adm1272 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "adm1272: {}", &self.device)
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Read16(Register, u16),
    ReadError(Register, ResponseCode),
    None,
}

ringbuf!(Trace, 32, Trace::None);

impl Adm1272 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
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

    pub fn read_vin(&self) -> Result<Volts, Error> {
        let vin = self.read_reg16(Register::PMBus(Command::ReadVIn))?;

        Ok(Volts(0.0))
    }
}
