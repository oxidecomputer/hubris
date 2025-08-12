// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the MAX31790 fan controller

use crate::Validate;
use bitfield::bitfield;
use drv_i2c_api::*;
use ringbuf::*;
use userlib::{
    units::{PWMDuty, Rpm},
    FromPrimitive,
};

#[allow(dead_code)]
#[derive(Copy, Clone, Debug)]
pub enum I2cWatchdog {
    Disabled = 0b00,
    FiveSeconds = 0b01,
    TenSeconds = 0b10,
    ThirtySeconds = 0b11,
}

#[allow(clippy::enum_variant_names)]
#[derive(FromPrimitive)]
#[repr(u8)]
enum Frequency {
    F25Hz = 0b0000,
    F30Hz = 0b0001,
    F35Hz = 0b0010,
    F100Hz = 0b0011,
    F125Hz = 0b0100,
    F149_7Hz = 0b0101,
    F1250Hz = 0b0110,
    F1470Hz = 0b0111,
    F3570Hz = 0b1000,
    F5000Hz = 0b1001,
    F12500Hz = 0b1010,
    F25000Hz = 0b1011,
}

#[allow(clippy::enum_variant_names)]
#[allow(dead_code)]
enum SpinUp {
    NoSpinUp = 0b00,
    HalfSecond = 0b01,
    OneSecond = 0b10,
    TwoSeconds = 0b11,
}

bitfield! {
    pub struct GlobalConfiguration(u8);
    standby, set_standby: 7;
    reset, set_reset: 6;
    bus_timeout_disabled, set_bus_timeout_disabled: 5;
    external_oscillator, _: 3;
    i2c_watchdog, set_i2c_watchdog: 2, 1;
    i2c_watchdog_faulted, _: 0;
}

bitfield! {
    pub struct PWMFrequency(u8);
    pwm_46, _: 7, 4;
    pwm_13, _: 3, 0;
}

bitfield! {
    pub struct FanConfiguration(u8);
    rpm, set_rpm: 7;
    spinup, set_spinup: 6, 5;
    monitor_only, set_monitor_only: 4;
    tach_input_enable, set_tach_input_enable: 3;
    locked_rotor_enable, set_locked_rotor_enable: 2;
    locked_rotor_polarity_high, set_locked_rotor_polarity_high: 1;
    pwm_disable, set_pwm_dsable: 0;
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Register {
    GlobalConfiguration = 0x00,
    PWMFrequency = 0x01,
    Fan1Configuration = 0x02,
    Fan2Configuration = 0x03,
    Fan3Configuration = 0x04,
    Fan4Configuration = 0x05,
    Fan5Configuration = 0x06,
    Fan6Configuration = 0x07,
    Fan1Dynamics = 0x08,
    Fan2Dynamics = 0x09,
    Fan3Dynamics = 0x0a,
    Fan4Dynamics = 0x0b,
    Fan5Dynamics = 0x0c,
    Fan6Dynamics = 0x0d,
    UserByte0 = 0x0e,
    UserByte1 = 0x0f,
    FanFaultStatus2 = 0x10,
    FanFaultStatus1 = 0x11,
    FanFaultMask2 = 0x12,
    FanFaultMask1 = 0x13,
    FailedFanSequentialStart = 0x14,
    UserByte2 = 0x15,
    UserByte3 = 0x16,
    UserByte4 = 0x17,
    Tach1CountMSB = 0x18,
    Tach1CountLSB = 0x19,
    Tach2CountMSB = 0x1a,
    Tach2CountLSB = 0x1b,
    Tach3CountMSB = 0x1c,
    Tach3CountLSB = 0x1d,
    Tach4CountMSB = 0x1e,
    Tach4CountLSB = 0x1f,
    Tach5CountMSB = 0x20,
    Tach5CountLSB = 0x21,
    Tach6CountMSB = 0x22,
    Tach6CountLSB = 0x23,
    Tach7CountMSB = 0x24,
    Tach7CountLSB = 0x25,
    Tach8CountMSB = 0x26,
    Tach8CountLSB = 0x27,
    Tach9CountMSB = 0x28,
    Tach9CountLSB = 0x29,
    Tach10CountMSB = 0x2a,
    Tach10CountLSB = 0x2b,
    Tach11CountMSB = 0x2c,
    Tach11CountLSB = 0x2d,
    Tach12CountMSB = 0x2e,
    Tach12CountLSB = 0x2f,
    PWMOut1DutyCycleMSB = 0x30,
    PWMOut1DutyCycleLSB = 0x31,
    PWMOut2DutyCycleMSB = 0x32,
    PWMOut2DutyCycleLSB = 0x33,
    PWMOut3DutyCycleMSB = 0x34,
    PWMOut3DutyCycleLSB = 0x35,
    PWMOut4DutyCycleMSB = 0x36,
    PWMOut4DutyCycleLSB = 0x37,
    PWMOut5DutyCycleMSB = 0x38,
    PWMOut5DutyCycleLSB = 0x39,
    PWMOut6DutyCycleMSB = 0x3a,
    PWMOut6DutyCycleLSB = 0x3b,
    PWMOut1TargetDutyCycleMSB = 0x40,
    PWMOut1TargetDutyCycleLSB = 0x41,
    PWMOut2TargetDutyCycleMSB = 0x42,
    PWMOut2TargetDutyCycleLSB = 0x43,
    PWMOut3TargetDutyCycleMSB = 0x44,
    PWMOut3TargetDutyCycleLSB = 0x45,
    PWMOut4TargetDutyCycleMSB = 0x46,
    PWMOut4TargetDutyCycleLSB = 0x47,
    PWMOut5TargetDutyCycleMSB = 0x48,
    PWMOut5TargetDutyCycleLSB = 0x49,
    PWMOut6TargetDutyCycleMSB = 0x4a,
    PWMOut6TargetDutyCycleLSB = 0x4b,
    UserByte5 = 0x4c,
    UserByte6 = 0x4d,
    UserByte7 = 0x4e,
    UserByte8 = 0x4f,
    Tach1TargetCountMSB = 0x50,
    Tach1TargetCountLSB = 0x51,
    Tach2TargetCountMSB = 0x52,
    Tach2TargetCountLSB = 0x53,
    Tach3TargetCountMSB = 0x54,
    Tach3TargetCountLSB = 0x55,
    Tach4TargetCountMSB = 0x56,
    Tach4TargetCountLSB = 0x57,
    Tach5TargetCountMSB = 0x58,
    Tach5TargetCountLSB = 0x59,
    Tach6TargetCountMSB = 0x5a,
    Tach6TargetCountLSB = 0x5b,
    UserByte9 = 0x5c,
    UserByte10 = 0x5d,
    UserByte11 = 0x5e,
    UserByte12 = 0x5f,
    Window1 = 0x60,
    Window2 = 0x61,
    Window3 = 0x62,
    Window4 = 0x63,
    Window5 = 0x64,
    Window6 = 0x65,
    UserByte13 = 0x66,
    UserByte14 = 0x67,
}

pub struct Max31790 {
    pub device: I2cDevice,
}

pub const MAX_FANS: u8 = 6;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Fan(u8);

impl TryFrom<u8> for Fan {
    type Error = ();
    /// Fans are based on a 0-based index. This should *not* be the number
    /// of the fan (the fan numbers have a 1-based index)
    fn try_from(index: u8) -> Result<Self, Self::Error> {
        if index >= MAX_FANS {
            Err(())
        } else {
            Ok(Self(index))
        }
    }
}

impl Fan {
    fn register(&self, base: Register, shift: u8) -> Register {
        let addend = self.0 << shift;
        Register::from_u8((base as u8) + addend).unwrap()
    }

    fn configuration(&self) -> Register {
        self.register(Register::Fan1Configuration, 0)
    }

    fn tach_count(&self) -> Register {
        self.register(Register::Tach1CountMSB, 1)
    }

    fn pwm_target(&self) -> Register {
        self.register(Register::PWMOut1TargetDutyCycleMSB, 1)
    }
}

fn read_reg8(
    device: &I2cDevice,
    register: Register,
) -> Result<u8, ResponseCode> {
    device.read_reg::<u8, u8>(register as u8)
}

fn read_reg16(
    device: &I2cDevice,
    register: Register,
) -> Result<[u8; 2], ResponseCode> {
    device.read_reg::<u8, [u8; 2]>(register as u8)
}

fn write_reg8(
    device: &I2cDevice,
    register: Register,
    val: u8,
) -> Result<(), ResponseCode> {
    device.write(&[register as u8, val])
}

fn write_reg16(
    device: &I2cDevice,
    register: Register,
    val: u16,
) -> Result<(), ResponseCode> {
    device.write(&[register as u8, (val >> 8) as u8, (val & 0xff) as u8])
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    ZeroTach(Fan),
}

ringbuf!(Trace, 6, Trace::None);

impl Max31790 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn initialize(&self) -> Result<(), ResponseCode> {
        let device = &self.device;

        let mut config = GlobalConfiguration(read_reg8(
            device,
            Register::GlobalConfiguration,
        )?);
        config.set_i2c_watchdog(I2cWatchdog::Disabled as u8);
        write_reg8(device, Register::GlobalConfiguration, config.0)?;

        for fan in 0..MAX_FANS {
            let fan = Fan::try_from(fan).unwrap();
            let reg = fan.configuration();

            let mut config = FanConfiguration(read_reg8(device, reg)?);
            config.set_tach_input_enable(true);

            write_reg8(device, reg, config.0)?;
            write_reg8(device, fan.pwm_target(), 0)?;
        }

        Ok(())
    }

    /// Determines the rotations per minute based on the tach count
    pub fn fan_rpm(&self, fan: Fan) -> Result<Rpm, ResponseCode> {
        let val = read_reg16(&self.device, fan.tach_count())?;

        //
        // The tach count is somewhat misnamed: it is in fact the number of
        // 8192 Hz clock cycles counted in a configurable number of pulses of
        // the tach.  (It would be more aptly named a pulse count.) The number
        // of pulses (NP) per revolution of the fan is specific to the fan,
        // but is generally two for the DC brushless fans we care about.  The
        // number of pulses of the tach measured is called the Speed Range
        // (SR) and defaults to 4.
        //
        // So to get from the tach count to the time per revolution:
        //
        //                    count * NP
        //                t = ----------
        //                    8192 * SR
        //
        // And to get from there to RPM, we want to divide 60 by t:
        //
        //                   60 * 8192 * SR
        //   RPM = 60 / t =  --------------
        //                     count * NP
        //
        let count = ((val[0] as u32) << 3) | (val[1] >> 5) as u32;

        const TACH_POR_VALUE: u32 = 0b111_1111_1111;
        const SR: u32 = 4;
        const NP: u32 = 2;
        const FREQ: u32 = 8192;

        if count == 0 {
            //
            // We don't really expect this:  generally, if a fan is off (or is
            // otherwise emiting non-detectable tach input pulses), the
            // controller will report the power-on-reset value for the tach
            // count, not 0.  So if we see a zero count, we will assume that
            // this is an error rather than a 0 RPM reading, and record it to
            // a (small) ring buffer and return accordingly.
            //
            ringbuf_entry!(Trace::ZeroTach(fan));
            Err(ResponseCode::BadDeviceState)
        } else if count == TACH_POR_VALUE {
            Ok(Rpm(0))
        } else {
            let rpm = (60 * FREQ * SR) / (count * NP);
            Ok(Rpm(rpm as u16))
        }
    }

    /// Set the PWM duty cycle for a fan
    pub fn set_pwm(&self, fan: Fan, pwm: PWMDuty) -> Result<(), ResponseCode> {
        let perc = core::cmp::min(pwm.0, 100) as f32;

        let val = ((perc / 100.0) * 0b1_1111_1111 as f32) as u16;
        write_reg16(&self.device, fan.pwm_target(), val << 7)
    }

    pub fn set_watchdog(&self, wd: I2cWatchdog) -> Result<(), ResponseCode> {
        let mut config = GlobalConfiguration(read_reg8(
            &self.device,
            Register::GlobalConfiguration,
        )?);
        config.set_i2c_watchdog(wd as u8);
        write_reg8(&self.device, Register::GlobalConfiguration, config.0)
    }
}

impl Validate<ResponseCode> for Max31790 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        //
        // The device doesn't have an identity register per se; to validate it,
        // we make sure that the PWM Frequency register contains valid
        // frequencies -- which doesn't eliminate many possibilities, but is
        // better than nothing.
        //
        let freq = PWMFrequency(read_reg8(device, Register::PWMFrequency)?);

        let pwm_13 = Frequency::from_u8(freq.pwm_13());
        let pwm_46 = Frequency::from_u8(freq.pwm_46());

        Ok(pwm_13.is_some() && pwm_46.is_some())
    }
}
