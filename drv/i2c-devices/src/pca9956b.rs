// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the PCA9956B LED driver

use crate::Validate;
use drv_i2c_api::{I2cDevice, ResponseCode};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, Ord, PartialOrd)]
pub enum Register {
    MODE1 = 0x00,
    MODE2 = 0x01,
    LEDOUT0 = 0x02,
    LEDOUT1 = 0x03,
    LEDOUT2 = 0x04,
    LEDOUT3 = 0x05,
    LEDOUT4 = 0x06,
    LEDOUT5 = 0x07,
    GRPPWM = 0x08,
    GRPFREQ = 0x09,
    PWM0 = 0x0A,
    PWM1 = 0x0B,
    PWM2 = 0x0C,
    PWM3 = 0x0D,
    PWM4 = 0x0E,
    PWM5 = 0x0F,
    PWM6 = 0x10,
    PWM7 = 0x11,
    PWM8 = 0x12,
    PWM9 = 0x13,
    PWM10 = 0x14,
    PWM11 = 0x15,
    PWM12 = 0x16,
    PWM13 = 0x17,
    PWM14 = 0x18,
    PWM15 = 0x19,
    PWM16 = 0x1A,
    PWM17 = 0x1B,
    PWM18 = 0x1C,
    PWM19 = 0x1D,
    PWM20 = 0x1E,
    PWM21 = 0x1F,
    PWM22 = 0x20,
    PWM23 = 0x21,
    IREF0 = 0x22,
    IREF1 = 0x23,
    IREF2 = 0x24,
    IREF3 = 0x25,
    IREF4 = 0x26,
    IREF5 = 0x27,
    IREF6 = 0x28,
    IREF7 = 0x29,
    IREF8 = 0x2A,
    IREF9 = 0x2B,
    IREF10 = 0x2C,
    IREF11 = 0x2D,
    IREF12 = 0x2E,
    IREF13 = 0x2F,
    IREF14 = 0x30,
    IREF15 = 0x31,
    IREF16 = 0x32,
    IREF17 = 0x33,
    IREF18 = 0x34,
    IREF19 = 0x35,
    IREF20 = 0x36,
    IREF21 = 0x37,
    IREF22 = 0x38,
    IREF23 = 0x39,
    OFFSET = 0x3A,
    SUBADR1 = 0x3B,
    SUBADR2 = 0x3C,
    SUBADR3 = 0x3D,
    ALLCALLADR = 0x3E,
    PWMALL = 0x3F,
    IREFALL = 0x40,
    EFLAG0 = 0x41,
    EFLAG1 = 0x42,
    EFLAG2 = 0x43,
    EFLAG3 = 0x44,
    EFLAG4 = 0x45,
    EFLAG5 = 0x46,
}

/// The auto-increment feature of the PCA9956B's internal address will only go
/// up to 0x3E (ALLCALLADR) at its highest configuration. Attempting to
/// auto-increment outside a range (as specified by Table 6 in the datasheet)
/// will not work.
const MAX_AUTO_INC_REG: Register = Register::ALLCALLADR;
const MAX_BUF_SIZE: usize = MAX_AUTO_INC_REG as usize;

/// ERR representations per Table 21 of the datasheet
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum LedErr {
    #[default]
    NoError = 0b00,
    ShortCircuit = 0b01,
    OpenCircuit = 0b10,
    Invalid = 0b11,
}

impl From<u8> for LedErr {
    fn from(i: u8) -> Self {
        match i {
            0 => LedErr::NoError,
            1 => LedErr::ShortCircuit,
            2 => LedErr::OpenCircuit,
            _ => LedErr::Invalid,
        }
    }
}

/// Pca9956BErrorState is used to summarize the types of errors seen
/// Overtemp is true if MODE2_OVERTEMP was 1
/// The other u32 fields are masks of which LED output the error was observed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Pca9956BErrorState {
    pub overtemp: bool,
    pub errors: [LedErr; NUM_LEDS],
}

/// Auto-increment flag is Bit 7 of the control register. Bits 6..0 are address.
const CTRL_AUTO_INCR_MASK: u8 = 1 << 7;
/// The MODE2 OVERTEMP bit indicates if an overtempature condition has occurred
const MODE2_OVERTEMP_MASK: u8 = 1 << 7;
/// The MODE2 ERROR bit indicates if any error conditions are in EFLAGn
const MODE2_ERROR_MASK: u8 = 1 << 6;
/// The MODE2 CLRERR bit clears all error conditions in EFLAGn
const MODE2_CLRERR_MASK: u8 = 1 << 4;
/// The MODE2 reserved bits have a defined pattern (0b101) and are read only.
/// They will be used to validate a PCA9956B with humility
const MODE2_RSVD_MASK: u8 = 0x7;
const MODE2_RSVD: u8 = 0x5;

pub struct Pca9956B {
    device: I2cDevice,
}

pub const NUM_LEDS: usize = 24;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The low-level I2C communication returned an error
    I2cError(ResponseCode),

    /// The LED index is too large
    InvalidLED(u8),

    /// Write buffer too large
    WriteBufferTooLarge(usize),

    /// Register is outside a valid auto-increment range
    InvalidAutoIncReg(Register),
}

impl From<ResponseCode> for Error {
    fn from(err: ResponseCode) -> Self {
        Error::I2cError(err)
    }
}

impl Pca9956B {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    /// Read a single Register
    fn read_reg(&self, reg: Register) -> Result<u8, Error> {
        self.device
            .read_reg::<u8, u8>(reg as u8)
            .map_err(Error::I2cError)
    }

    /// Read a number of Registers into `buf`
    /// Note that CTRL_AUTO_INCR_MASK is set here to enable the device's
    /// auto-increment feature, which has varying behaviors and limitations that
    /// must be accounted for.
    #[allow(dead_code)]
    fn read_buffer(&self, reg: Register, buf: &mut [u8]) -> Result<(), Error> {
        if buf.len() > MAX_BUF_SIZE {
            return Err(Error::WriteBufferTooLarge(buf.len()));
        } else if reg > MAX_AUTO_INC_REG {
            return Err(Error::InvalidAutoIncReg(reg));
        }

        self.device
            .read_reg_into((reg as u8) | CTRL_AUTO_INCR_MASK, buf)
            .map_err(Error::I2cError)?;

        Ok(())
    }

    /// Write a single Register
    fn write_reg(&self, reg: Register, val: u8) -> Result<(), Error> {
        let buffer = [reg as u8, val];
        self.device.write(&buffer).map_err(Error::I2cError)
    }

    /// Write a number of Registers into `buf`
    /// Note that CTRL_AUTO_INCR_MASK is set here to enable the device's
    /// auto-increment feature, which has varying behaviors and limitations that
    /// must be accounted for.
    fn write_buffer(&self, reg: Register, buf: &[u8]) -> Result<(), Error> {
        if buf.len() > MAX_BUF_SIZE {
            return Err(Error::WriteBufferTooLarge(buf.len()));
        } else if reg > MAX_AUTO_INC_REG {
            return Err(Error::InvalidAutoIncReg(reg));
        }

        let mut data: [u8; MAX_BUF_SIZE + 1] = [0; MAX_BUF_SIZE + 1];
        data[0] = (reg as u8) | CTRL_AUTO_INCR_MASK;
        data[1..=buf.len()].copy_from_slice(buf);

        self.device
            .write(&data[..=buf.len()])
            .map_err(Error::I2cError)
    }

    /// Sets the device's IREFALL register to `val`
    /// IREFALL gets copied into all IREFn registers by the device.
    pub fn set_iref_all(&self, val: u8) -> Result<(), Error> {
        self.write_reg(Register::IREFALL, val)
    }

    /// Sets the device's PWMALL register to `val`
    /// PWMALL gets copied into all PWMn registers by the device.
    pub fn set_pwm_all(&self, val: u8) -> Result<(), Error> {
        self.write_reg(Register::PWMALL, val)
    }

    /// Sets the PWMx register to `val` for a given `led`
    pub fn set_a_led_pwm(&self, led: u8, val: u8) -> Result<(), Error> {
        if led >= NUM_LEDS as u8 {
            return Err(Error::InvalidLED(led));
        }
        let reg = Register::from_u8((Register::PWM0 as u8) + led).unwrap();
        self.write_reg(reg, val)
    }

    /// Sets the PWMx register a number of LEDs, up to NUM_LEDS, beginning with
    /// PWM0.
    pub fn set_all_led_pwm(&self, vals: &[u8]) -> Result<(), Error> {
        if vals.len() > NUM_LEDS {
            return Err(Error::InvalidLED(NUM_LEDS as u8));
        }
        self.write_buffer(Register::PWM0, vals)
    }

    /// Queries the MODE2 register and to check the OVERTEMP and ERROR bits
    /// If ERROR is set, each EFLAGx register will be read and parsed.
    /// An important thing to note about this device is that in order to do
    /// error detection (reflected in EFLAGx) the PWMx associated with the LED
    /// must be higher than 0x08. So if the PWMx is set to zero, no error can
    /// be detected.
    pub fn check_errors(&self) -> Result<Pca9956BErrorState, Error> {
        let mode2 = self.read_reg(Register::MODE2)?;
        let overtemp = (mode2 & MODE2_OVERTEMP_MASK) != 0;
        let error = (mode2 & MODE2_ERROR_MASK) != 0;

        let mut err_state = Pca9956BErrorState {
            overtemp,
            ..Default::default()
        };

        if error {
            let mut eflags: [u8; 6] = [0; 6];
            for (i, eflagx) in eflags.iter_mut().enumerate() {
                // Notably, the auto-increment function does not apply to these
                // registers, so they must be fetched individually
                let reg = Register::from_u8(Register::EFLAG0 as u8 + i as u8)
                    .unwrap();
                *eflagx = self.read_reg(reg)?;
            }

            // Convert the EFLAGx contents into LedErr values
            for (i, eflagx) in eflags.iter_mut().enumerate() {
                let eflag = *eflagx;
                for j in 0..=3 {
                    let led_idx = (i * 4) + j;
                    err_state.errors[led_idx] =
                        LedErr::from((eflag >> (j * 2)) & 0b11);
                }
            }

            self.write_reg(Register::MODE2, mode2 | MODE2_CLRERR_MASK)?;
        }

        Ok(err_state)
    }
}

// The PCA9956B does not expose anything like a unique ID or manufacturer code,
// which is the type of information we typically like to validate against.
// MODE2[2:0] are set to read only an initialized to b101, so use that to
// validate.
impl Validate<ResponseCode> for Pca9956B {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        let mode = Pca9956B::new(device).read_reg(Register::MODE2).map_err(
            |e| match e {
                // read_reg can only return Error::I2cError
                Error::I2cError(e) => e,
                _ => panic!(),
            },
        )?;

        Ok(mode & MODE2_RSVD_MASK == MODE2_RSVD)
    }
}
