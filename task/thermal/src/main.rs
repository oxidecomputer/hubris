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

use crate::{bsp::Bsp, control::ThermalControl};
use core::convert::TryFrom;
use drv_i2c_api::ResponseCode;
pub use drv_i2c_devices::max31790::Fan;
use drv_i2c_devices::max31790::Max31790;
use drv_i2c_devices::TempSensor;
use drv_i2c_devices::{
    sbtsi::Sbtsi, tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use idol_runtime::{NotificationHandler, RequestError};
use task_thermal_api::{ThermalError, ThermalMode};
use userlib::units::*;
use userlib::*;

use task_sensor_api::{Sensor as SensorApi, SensorId};

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

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
    fn read_temp(&mut self) -> Result<Celsius, ResponseCode> {
        let t = match &mut self.device {
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

struct ServerImpl<'a> {
    mode: ThermalMode,
    control: ThermalControl<'a>,
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl<'a> ServerImpl<'a> {
    /// Configures the control loop to run in manual mode, loading the given
    /// PWM value immediately to all fans.
    ///
    /// Returns an error if the PWM code is invalid (> 100) or communication
    /// with any fan fails.
    fn set_mode_manual(
        &mut self,
        initial_pwm: PWMDuty,
    ) -> Result<(), ThermalError> {
        self.mode = ThermalMode::Manual;
        self.control.outputs.set_pwm(initial_pwm)
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
        self.mode = ThermalMode::Auto;
        self.control.reset(initial_pwm)
    }

    /// Attempt to drop into failsafe mode, with all fans at 100%.
    ///
    /// Crashes the task on failure, because there's not much we can do
    /// for recovery if this fails.
    fn set_mode_failsafe(&mut self) {
        self.mode = ThermalMode::Failsafe;
        self.control.outputs.set_pwm(PWMDuty(100)).unwrap();
    }
}

impl<'a> idl::InOrderThermalImpl for ServerImpl<'a> {
    fn set_fan_pwm(
        &mut self,
        _: &RecvMessage,
        index: u8,
        pwm: PWMDuty,
    ) -> Result<(), RequestError<ThermalError>> {
        if self.mode != ThermalMode::Manual {
            return Err(ThermalError::NotInManualMode.into());
        }
        if let Ok(fan) = Fan::try_from(index) {
            self.control
                .outputs
                .set_fan_pwm(fan, pwm)
                .map_err(|_| ThermalError::DeviceError.into())
        } else {
            Err(ThermalError::InvalidFan.into())
        }
    }

    fn set_mode_manual(
        &mut self,
        _: &RecvMessage,
        initial_pwm: PWMDuty,
    ) -> Result<(), RequestError<ThermalError>> {
        (self as &mut ServerImpl)
            .set_mode_manual(initial_pwm)
            .map_err(Into::into)
    }

    fn set_mode_auto(
        &mut self,
        _: &RecvMessage,
        initial_pwm: PWMDuty,
    ) -> Result<(), RequestError<ThermalError>> {
        self.mode = ThermalMode::Auto;
        (self as &mut ServerImpl)
            .set_mode_auto(initial_pwm)
            .map_err(Into::into)
    }
}

impl<'a> NotificationHandler for ServerImpl<'a> {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), TIMER_MASK);

        match self.mode {
            ThermalMode::Auto => {
                if self.control.run_control().is_err() {
                    self.set_mode_failsafe();
                }
            }
            ThermalMode::Off | ThermalMode::Manual | ThermalMode::Failsafe => {
                // Ignore read errors, since the control loop isn't actually
                // running in this mode.
                let _ = self.control.read_sensors();
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let sensor_api = SensorApi::from(SENSOR.get_task_id());

    let mut bsp = Bsp::new(i2c_task);
    let control = bsp.controller(sensor_api);

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        mode: ThermalMode::Off,
        control,
        deadline,
    };
    let mut buffer = [0; idl::INCOMING_SIZE];

    server.set_mode_manual(PWMDuty(80)).unwrap();

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{PWMDuty, ThermalError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
