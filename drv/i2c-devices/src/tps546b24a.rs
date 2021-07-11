//! Driver for the TPS546B24A buck converter

use drv_i2c_api::*;
use pmbus::*;
use ringbuf::*;
use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Register {
    PMBus(Command),
}

impl From<Register> for u8 {
    fn from(value: Register) -> Self {
        match value {
            Register::PMBus(cmd) => cmd as u8,
        }
    }
}

pub struct Tps546b24a {
    device: I2cDevice,
    exponent: Option<ULinear16Exponent>,
}

impl core::fmt::Display for Tps546b24a {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tps546b24a: {}", &self.device)
    }
}

#[derive(Debug)]
pub enum Error {
    BadRead { reg: Register, code: ResponseCode },
    BadWrite { reg: Register, code: ResponseCode },
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Read8(Register, u8),
    Read16(Register, u16),
    Write16(Register, u8, u8),
    ReadError(Register, ResponseCode),
    WriteError(Register, ResponseCode),
    None,
}

ringbuf!(Trace, 32, Trace::None);

impl Tps546b24a {
    pub fn new(device: &I2cDevice) -> Self {
        Tps546b24a {
            device: *device,
            exponent: None,
        }
    }

    fn read_reg8(&self, register: Register) -> Result<u8, Error> {
        let rval = self.device.read_reg::<u8, u8>(u8::from(register));

        match rval {
            Ok(val) => {
                ringbuf_entry!(Trace::Read8(register, val));
                Ok(val)
            }

            Err(code) => {
                ringbuf_entry!(Trace::ReadError(register, code));
                Err(Error::BadRead {
                    reg: register,
                    code: code,
                })
            }
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
                Err(Error::BadRead {
                    reg: register,
                    code: code,
                })
            }
        }
    }

    #[allow(dead_code)]
    fn write_reg16(&self, register: Register, value: u16) -> Result<(), Error> {
        let v = value.to_be_bytes();
        ringbuf_entry!(Trace::Write16(register, v[0], v[1]));

        match self.device.write(&[u8::from(register), v[0], v[1]]) {
            Err(code) => {
                ringbuf_entry!(Trace::WriteError(register, code));
                Err(Error::BadWrite {
                    reg: register,
                    code: code,
                })
            }

            Ok(_) => Ok(()),
        }
    }

    fn read_exponent(&mut self) -> Result<ULinear16Exponent, Error> {
        Ok(match self.exponent {
            None => {
                let mode =
                    self.read_reg8(Register::PMBus(Command::VOUT_MODE))?;

                if let VOutMode::ULinear16(exp) = mode.into() {
                    self.exponent = Some(exp);
                    exp
                } else {
                    panic!("expected ULinear16 VOUT_MODE");
                }
            }
            Some(exp) => exp,
        })
    }

    pub fn read_vout(&mut self) -> Result<Volts, Error> {
        let vout = self.read_reg16(Register::PMBus(Command::READ_VOUT))?;
        Ok(Volts(ULinear16(vout, self.read_exponent()?).to_real()))
    }

    pub fn read_iout(&mut self) -> Result<Amperes, Error> {
        let iout = self.read_reg16(Register::PMBus(Command::READ_IOUT))?;
        Ok(Amperes(Linear11(iout).to_real()))
    }
}
