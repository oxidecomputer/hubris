//! Driver for the MAX31790 fan controller

use bitfield::bitfield;
use drv_i2c_api::*;
use ringbuf::*;
use userlib::units::*;
use userlib::*;

#[allow(dead_code)]
enum I2cWatchdog {
    Disabled = 0b00,
    FiveSeconds = 0b01,
    TenSeconds = 0b10,
    ThirtySeconds = 0b11,
}

#[allow(dead_code)]
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
#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
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

#[derive(Copy, Clone, Debug)]
pub enum Error {
    BadRead8 { reg: Register, code: ResponseCode },
    BadRead16 { reg: Register, code: ResponseCode },
    BadWrite { reg: Register, code: ResponseCode },
    IllegalFan,
}

pub struct Max31790 {
    pub device: I2cDevice,
}

impl core::fmt::Display for Max31790 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "max31790: {}", &self.device)
    }
}

pub const FAN_MIN: u8 = 1;
pub const FAN_MAX: u8 = 6;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Fan(pub u8);

impl Fan {
    fn register(&self, base: Register, shift: u8) -> Result<Register, Error> {
        if self.0 < FAN_MIN || self.0 > FAN_MAX {
            Err(Error::IllegalFan)
        } else {
            let addend = (self.0 - FAN_MIN) << shift;
            Ok(Register::from_u8((base as u8) + addend).unwrap())
        }
    }

    fn configuration(&self) -> Result<Register, Error> {
        self.register(Register::Fan1Configuration, 0)
    }

    fn tach_count(&self) -> Result<Register, Error> {
        self.register(Register::Tach1CountMSB, 1)
    }

    fn pwm_target(&self) -> Result<Register, Error> {
        self.register(Register::PWMOut1TargetDutyCycleMSB, 1)
    }
}

ringbuf!(
    (Option<Register>, Result<[u8; 2], ResponseCode>),
    32,
    (None, Ok([0, 0]))
);

fn read_reg8(device: &I2cDevice, register: Register) -> Result<u8, Error> {
    let rval = device.read_reg::<u8, u8>(register as u8);

    match rval {
        Ok(val) => {
            ringbuf_entry!((Some(register), Ok([val, 0])));
            Ok(val)
        }

        Err(code) => {
            ringbuf_entry!((Some(register), Err(code)));
            Err(Error::BadRead8 {
                reg: register,
                code: code,
            })
        }
    }
}

fn read_reg16(
    device: &I2cDevice,
    register: Register,
) -> Result<[u8; 2], Error> {
    let rval = device.read_reg::<u8, [u8; 2]>(register as u8);

    ringbuf_entry!((Some(register), rval));

    match rval {
        Ok(val) => Ok(val),
        Err(code) => Err(Error::BadRead16 {
            reg: register,
            code: code,
        }),
    }
}

fn write_reg(
    device: &I2cDevice,
    register: Register,
    val: u8,
) -> Result<(), Error> {
    let rval = device.write(&[register as u8, val]);

    match rval {
        Ok(_) => {
            ringbuf_entry!((Some(register), Ok([val.into(), 0])));
            Ok(())
        }
        Err(code) => {
            ringbuf_entry!((Some(register), Err(code)));
            Err(Error::BadWrite {
                reg: register,
                code: code,
            })
        }
    }
}

impl Max31790 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn initialize(&self) -> Result<(), Error> {
        let device = &self.device;

        let _config = GlobalConfiguration(read_reg8(
            device,
            Register::GlobalConfiguration,
        )?);

        for fan in FAN_MIN..=FAN_MAX {
            let fan = Fan(fan);
            let reg = fan.configuration()?;

            let mut config = FanConfiguration(read_reg8(device, reg)?);
            config.set_tach_input_enable(true);

            write_reg(device, reg, config.0)?;
            write_reg(device, fan.pwm_target()?, 0)?;
        }

        Ok(())
    }

    pub fn fan_rpm(&self, fan: Fan) -> Result<Rpm, Error> {
        let val = read_reg16(&self.device, fan.tach_count()?)?;

        let count = ((val[0] as u32) << 3) | (val[1] >> 5) as u32;

        if count == 0b111_1111_1111 {
            Ok(Rpm(0))
        } else {
            //
            // sr of 4 is the default. np is fan-specific, but seems to be two
            // for the fans we care about.
            //
            let np = 2;
            let sr = 4;

            let rpm = (60 * sr * 8192) / (np * count);

            Ok(Rpm(rpm as u16))
        }
    }
}
