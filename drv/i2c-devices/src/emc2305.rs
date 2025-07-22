// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the EMC2305 fan controller

use crate::Validate;
use bitfield::bitfield;
use drv_i2c_api::*;
use ringbuf::*;
use userlib::{
    units::{PWMDuty, Rpm},
    FromPrimitive, UnwrapLite,
};

bitfield! {
    pub struct Configuration(u8);
    mask, set_mask: 7;
    disable_timeout, set_disable_timeout: 6;
    watchdog_enable, set_watchdog_enable: 6;
    drive_clk, set_drive_clk: 1;
    use_clk, set_use_clk: 0;
}

bitfield! {
    pub struct FanCfg1(u8);
    closed_loop, set_closed_loop: 7;
    range, set_range: 6, 5;
    edges, set_edges: 4, 3;
    update_time, set_update_time: 2, 1, 0;
}

bitfield! {
    pub struct FanCfg2(u8);
    enable_ramp_control, set_enable_ramp_control: 6;
    enable_glitch_filter, set_enable_glitch_filter: 5;
    derivative_options, set_derivative_options: 4, 3;
    error_window, set_error_window: 2, 1;
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Register {
    Configuration = 0x20,
    FanStatus = 0x24,
    FanStallStatus = 0x28,
    FanSpinStatus = 0x26,
    DriveFailStatus = 0x27,
    FanInterruptEnable = 0x29,
    PwmPolarityCfg = 0x2a,
    PwmOutputCfg = 0x2b,
    PwmBaseF45 = 0x2c,
    PwmBaseF123 = 0x2d,

    Fan1Setting = 0x30,
    Pwm1Divide = 0x31,
    Fan1Cfg1 = 0x32,
    Fan1Cfg2 = 0x33,
    Gain1 = 0x35,
    Fan1SpinUpCfg = 0x36,
    Fan1MaxStep = 0x37,
    Fan1MinDrive = 0x38,
    Fan1ValidTach = 0x39,
    Fan1DriveFailBandLo = 0x3a,
    Fan1DriveFailBandHi = 0x3b,
    Tach1TargetLo = 0x3c,
    Tach1TargetHi = 0x3d,
    Tach1ReadingHi = 0x3e,
    Tach1ReadingLo = 0x3f,

    Fan2Setting = 0x40,
    Pwm2Divide = 0x41,
    Fan2Cfg1 = 0x42,
    Fan2Cfg2 = 0x43,
    Gain2 = 0x45,
    Fan2SpinUpCfg = 0x46,
    Fan2MaxStep = 0x47,
    Fan2MinDrive = 0x48,
    Fan2ValidTach = 0x49,
    Fan2DriveFailBandLo = 0x4a,
    Fan2DriveFailBandHi = 0x4b,
    Tach2TargetLo = 0x4c,
    Tach2TargetHi = 0x4d,
    Tach2ReadingHi = 0x4e,
    Tach2ReadingLo = 0x4f,

    Fan3Setting = 0x50,
    Pwm3Divide = 0x51,
    Fan3Cfg1 = 0x52,
    Fan3Cfg2 = 0x53,
    Gain3 = 0x55,
    Fan3SpinUpCfg = 0x56,
    Fan3MaxStep = 0x57,
    Fan3MinDrive = 0x58,
    Fan3ValidTach = 0x59,
    Fan3DriveFailBandLo = 0x5a,
    Fan3DriveFailBandHi = 0x5b,
    Tach3TargetLo = 0x5c,
    Tach3TargetHi = 0x5d,
    Tach3ReadingHi = 0x5e,
    Tach3ReadingLo = 0x5f,

    Fan4Setting = 0x60,
    Pwm4Divide = 0x61,
    Fan4Cfg1 = 0x62,
    Fan4Cfg2 = 0x63,
    Gain4 = 0x65,
    Fan4SpinUpCfg = 0x66,
    Fan4MaxStep = 0x67,
    Fan4MinDrive = 0x68,
    Fan4ValidTach = 0x69,
    Fan4DriveFailBandLo = 0x6a,
    Fan4DriveFailBandHi = 0x6b,
    Tach4TargetLo = 0x6c,
    Tach4TargetHi = 0x6d,
    Tach4ReadingHi = 0x6e,
    Tach4ReadingLo = 0x6f,

    Fan5Setting = 0x70,
    Pwm5Divide = 0x71,
    Fan5Cfg1 = 0x72,
    Fan5Cfg2 = 0x73,
    Gain5 = 0x75,
    Fan5SpinUpCfg = 0x76,
    Fan5MaxStep = 0x77,
    Fan5MinDrive = 0x78,
    Fan5ValidTach = 0x79,
    Fan5DriveFailBandLo = 0x7a,
    Fan5DriveFailBandHi = 0x7b,
    Tach5TargetLo = 0x7c,
    Tach5TargetHi = 0x7d,
    Tach5ReadingHi = 0x7e,
    Tach5ReadingLo = 0x7f,

    SoftwareLock = 0xef,
    ProductFeatures = 0xfc,
    ProductId = 0xfd,
    MfgId = 0xfe,
    Revision = 0xff,
}

pub struct Emc2305 {
    pub device: I2cDevice,
}

pub const MAX_FANS: u8 = 5;

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
    fn register(&self, base: Register) -> Register {
        Register::from_u8((base as u8) + self.0 * 0x10).unwrap_lite()
    }

    fn configuration1(&self) -> Register {
        self.register(Register::Fan1Cfg1)
    }

    fn configuration2(&self) -> Register {
        self.register(Register::Fan1Cfg2)
    }

    fn tach_count(&self) -> Register {
        self.register(Register::Tach1ReadingHi)
    }

    fn pwm_target(&self) -> Register {
        self.register(Register::Fan1Setting)
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

#[allow(unused)]
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
    TachOverflow(u32),
    BadFanCount(u8),
}

ringbuf!(Trace, 6, Trace::None);

impl Emc2305 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn initialize(&self, fan_count: u8) -> Result<(), ResponseCode> {
        if fan_count > MAX_FANS {
            ringbuf_entry!(Trace::BadFanCount(fan_count));
            return Err(ResponseCode::BadArg);
        }

        assert!(fan_count <= MAX_FANS);
        let device = &self.device;

        // Enable the watchdog at all times
        let mut config =
            Configuration(read_reg8(device, Register::Configuration)?);
        config.set_watchdog_enable(true);
        write_reg8(device, Register::Configuration, config.0)?;

        for fan in 0..fan_count {
            let fan = Fan::try_from(fan).unwrap_lite();

            // Configure tach stuff
            let reg1 = fan.configuration1();
            let mut cfg1 = FanCfg1(read_reg8(device, reg1)?);
            cfg1.set_range(0b01); // 1000 RPM minimum, TACH count multiple = 2
            cfg1.set_edges(0b01); // 5 edges sampled, TACH multiple = 1x
            write_reg8(device, reg1, cfg1.0)?;

            // Enable ramp control, to avoid the fans going from 0-100%
            let reg2 = fan.configuration2();
            let mut cfg2 = FanCfg2(read_reg8(device, reg2)?);
            cfg2.set_enable_ramp_control(true);
            write_reg8(device, reg2, cfg2.0)?;

            write_reg8(device, fan.pwm_target(), 0)?;
        }

        Ok(())
    }

    /// Determines the rotations per minute based on the tach count
    pub fn fan_rpm(&self, fan: Fan) -> Result<Rpm, ResponseCode> {
        let val = read_reg16(&self.device, fan.tach_count())?;

        // See section 4.4.1 in the datasheet for details
        //
        // The tachometer reading represents the number of 32 KHz clock cycles
        // counted in a single revolution of the fan.
        //
        // So to get from the tach count to the time per revolution:
        //
        //              1         n - 1
        //      RPM = ----- x ------------- x f_TACH x 60
        //            poles    count * 1/m
        //
        // Our settings are the following:
        //   poles = 2
        //   n = edges measured (set to 5 in `cfg1` above)
        //   m = range multiplier (2x, per configuration above)
        //   f_TACH = tachometer measurement frequency (32 KHz)
        //   count = reading
        //
        // Doing the math, we end up with
        //   RPM = 7680000 / count
        //
        // The count field is a 12-bit value, shifted by 3 and split between the
        // high and low bytes (see TACHOMETER READING HIGH BYTE REGISTER and
        // TACHOMETER READING LOW BYTE REGISTER in the datasheet).
        let count = ((val[0] as u32) << 5) | (val[1] >> 3) as u32;

        // If the fan isn't spinning or is disconnected, this is the default
        // value (dunno why the lowest bit is 0 instead of 1).
        const TACH_POR_VALUE: u32 = 0x1ffe;

        if count == 0 {
            ringbuf_entry!(Trace::ZeroTach(fan));
            return Err(ResponseCode::BadDeviceState);
        } else if count == TACH_POR_VALUE {
            return Ok(Rpm(0));
        }

        let rpm = 7_680_000 / count;
        let Ok(rpm) = rpm.try_into() else {
            ringbuf_entry!(Trace::TachOverflow(count));
            return Err(ResponseCode::BadDeviceState);
        };
        Ok(Rpm(rpm))
    }

    /// Set the PWM duty cycle for a fan
    pub fn set_pwm(&self, fan: Fan, pwm: PWMDuty) -> Result<(), ResponseCode> {
        let perc = core::cmp::min(pwm.0, 100) as f32;

        let val = ((perc / 100.0) * 255.0) as u8;
        write_reg8(&self.device, fan.pwm_target(), val)
    }

    pub fn set_watchdog(&self, enabled: bool) -> Result<(), ResponseCode> {
        let mut config =
            Configuration(read_reg8(&self.device, Register::Configuration)?);
        config.set_watchdog_enable(enabled);
        write_reg8(&self.device, Register::Configuration, config.0)
    }
}

impl Validate<ResponseCode> for Emc2305 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        let pid = read_reg8(device, Register::ProductId)?;
        let mfg = read_reg8(device, Register::MfgId)?;

        // The datasheet has ambiguity about whether PID should be 1011_0100
        // or 0011_0100, but experimentally, it seems to be the latter.
        Ok(pid == 0b0011_0100 && mfg == 0x5D)
    }
}
