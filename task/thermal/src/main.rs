// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

mod bsp;
mod control;

use crate::{
    bsp::{Bsp, BspT},
    control::ThermalControl,
};
use core::convert::TryFrom;
use drv_i2c_api::ResponseCode;
pub use drv_i2c_devices::max31790::Fan;
use drv_i2c_devices::max31790::Max31790;
use drv_i2c_devices::TempSensor;
use drv_i2c_devices::{
    sbtsi::Sbtsi, tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use task_thermal_api::{ThermalError, ThermalMode};
use userlib::units::*;
use userlib::*;

use task_sensor_api::{Sensor as SensorApi, SensorId};

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    ThermalMode(ThermalMode),
    FanReadFailed(usize, ResponseCode),
    MiscReadFailed(usize, ResponseCode),
    SensorReadFailed(usize, ResponseCode),
    ControlPwm(u8),
}
ringbuf!(Trace, 32, Trace::None);

////////////////////////////////////////////////////////////////////////////////

/// Enum containing all of our temperature sensor types, so we can store them
/// generically in an array.
enum Device {
    Tmp117(Tmp117),
    T6Nic(Tmp451),
    CPU(Sbtsi),
    Dimm(Tse2004Av),
}

struct TemperatureSensor {
    device: Device,
    id: SensorId,
}

impl TemperatureSensor {
    fn read_temp(&self) -> Result<Celsius, ResponseCode> {
        let t = match &self.device {
            Device::Tmp117(dev) => dev.read_temperature()?,
            Device::CPU(dev) => dev.read_temperature()?,
            Device::T6Nic(dev) => dev.read_temperature()?,
            Device::Dimm(dev) => dev.read_temperature()?,
        };
        Ok(t)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Enum containing all of our fan controller types, so we can store them
/// generically in an array.
enum FanControl {
    Max31790(Max31790),
}

impl FanControl {
    fn set_pwm(&self, fan: Fan, pwm: PWMDuty) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m) => m.set_pwm(fan, pwm),
        }
    }
    pub fn fan_rpm(&self, fan: Fan) -> Result<Rpm, ResponseCode> {
        match self {
            Self::Max31790(m) => m.fan_rpm(fan),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl<'a, B> {
    mode: ThermalMode,
    control: ThermalControl<'a, B>,
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl<'a, B: BspT> ServerImpl<'a, B> {
    /// Configures the control loop to run in manual mode, loading the given
    /// PWM value immediately to all fans.
    ///
    /// Returns an error if the PWM code is invalid (> 100) or communication
    /// with any fan fails.
    fn set_mode_manual(
        &mut self,
        initial_pwm: PWMDuty,
    ) -> Result<(), ThermalError> {
        self.set_mode(ThermalMode::Manual);
        self.control.set_pwm(initial_pwm)
    }

    /// Configures the control loop to run in automatic mode.
    ///
    /// The fans will not change speed until the next controller update tick.
    ///
    /// Returns an error if the given PWM value is invalid.
    fn set_mode_auto(
        &mut self,
        initial_pwm: PWMDuty,
    ) -> Result<(), ThermalError> {
        self.set_mode(ThermalMode::Auto);
        self.control.reset(initial_pwm)
    }

    fn set_mode(&mut self, m: ThermalMode) {
        self.mode = m;
        ringbuf_entry!(Trace::ThermalMode(m));
    }
}

impl<'a, B: BspT> idl::InOrderThermalImpl for ServerImpl<'a, B> {
    fn set_fan_pwm(
        &mut self,
        _: &RecvMessage,
        index: u8,
        pwm: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        if self.mode != ThermalMode::Manual {
            return Err(ThermalError::NotInManualMode.into());
        }
        let pwm =
            PWMDuty::try_from(pwm).map_err(|_| ThermalError::InvalidPWM)?;
        if let Ok(fan) = Fan::try_from(index) {
            self.control
                .set_fan_pwm(fan, pwm)
                .map_err(|_| ThermalError::DeviceError.into())
        } else {
            Err(ThermalError::InvalidFan.into())
        }
    }

    fn set_mode_manual(
        &mut self,
        _: &RecvMessage,
        initial_pwm: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        // Delegate to inner function after doing type conversions
        let initial_pwm = PWMDuty::try_from(initial_pwm)
            .map_err(|_| ThermalError::InvalidPWM)?;
        (self as &mut ServerImpl<B>)
            .set_mode_manual(initial_pwm)
            .map_err(Into::into)
    }

    fn set_mode_auto(
        &mut self,
        _: &RecvMessage,
        initial_pwm: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        // Delegate to inner function after doing type conversions
        let initial_pwm = PWMDuty::try_from(initial_pwm)
            .map_err(|_| ThermalError::InvalidPWM)?;
        (self as &mut ServerImpl<B>)
            .set_mode_auto(initial_pwm)
            .map_err(Into::into)
    }
}

impl<'a, B: BspT> NotificationHandler for ServerImpl<'a, B> {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), TIMER_MASK);

        match self.mode {
            ThermalMode::Auto => {
                // TODO: what to do with errors here?
                self.control.run_control();
            }
            ThermalMode::Manual => {
                // Ignore read errors, since the control loop isn't actually
                // running in this mode.
                let _ = self.control.read_sensors();
            }
            ThermalMode::Off => {
                panic!("Mode must not be 'Off' when server is running")
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let sensor_api = SensorApi::from(SENSOR.get_task_id());

    let mut bsp = Bsp::new(i2c_task);
    let control = ThermalControl::new(&mut bsp, sensor_api);

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        mode: ThermalMode::Off,
        control,
        deadline,
    };
    server.set_mode_manual(PWMDuty(0)).unwrap();

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::ThermalError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
