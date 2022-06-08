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
use drv_i2c_devices::max31790::I2cWatchdog;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use task_thermal_api::{ThermalError, ThermalMode};
use userlib::units::PWMDuty;
use userlib::*;

use task_sensor_api::Sensor as SensorApi;

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

struct ServerImpl<'a, B> {
    mode: ModeState<'a, B>,
    deadline: u64,
}

enum ModeState<'a, B> {
    /// We have not yet started the control loop, and have raw access to the
    /// BSP.
    NotStarted(&'a B),
    /// We have started the control loop.
    Started {
        /// Control loop state, which takes over the BSP once we're started all
        /// the way up.
        control: ThermalControl<'a, B>,
        /// Tracks whether we're in automatic mode. This is (ab)using a bool as
        /// a subset of `ThermalMode` so that we don't have to deal with the
        /// `Off` state, which shouldn't be observable here.
        auto: bool,
        /// Number of times we've woken, counting up to the next control loop
        /// iteration.
        counter: u64,
    },
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

/// How often to run the control loop, in multiples of TIMER_INTERVAL
const CONTROL_RATE: u64 = 10;

impl<B: BspT> ServerImpl<'_, B> {
    /// Configures the control loop to run in manual mode, loading the given
    /// PWM value immediately to all fans.
    ///
    /// Returns an error if the PWM code is invalid (> 100) or communication
    /// with any fan fails.
    fn set_mode_manual(
        &mut self,
        initial_pwm: PWMDuty,
    ) -> Result<(), ThermalError> {
        if let ModeState::Started { control, auto, .. } = &mut self.mode {
            *auto = false;
            ringbuf_entry!(Trace::ThermalMode(ThermalMode::Manual));
            control.set_pwm(initial_pwm)
        } else {
            Err(ThermalError::NotStarted.into())
        }
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
        if let ModeState::Started { control, auto, .. } = &mut self.mode {
            *auto = true;
            ringbuf_entry!(Trace::ThermalMode(ThermalMode::Auto));
            control.reset(initial_pwm)
        } else {
            Err(ThermalError::NotStarted.into())
        }
    }

    fn set_watchdog(&mut self, wd: I2cWatchdog) -> Result<(), ThermalError> {
        if let ModeState::Started { control, .. } = &mut self.mode {
            control
                .set_watchdog(wd)
                .map_err(|_| ThermalError::DeviceError)
        } else {
            Err(ThermalError::NotStarted.into())
        }
    }
}

impl<B: BspT> idl::InOrderThermalImpl for ServerImpl<'_, B> {
    fn set_fan_pwm(
        &mut self,
        _: &RecvMessage,
        index: u8,
        pwm: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        match &mut self.mode {
            ModeState::NotStarted(_) => Err(ThermalError::NotStarted.into()),
            ModeState::Started { auto: true, .. } => {
                Err(ThermalError::NotInManualMode.into())
            }
            ModeState::Started {
                auto: false,
                control,
                ..
            } => {
                let pwm = PWMDuty::try_from(pwm)
                    .map_err(|_| ThermalError::InvalidPWM)?;
                if let Ok(fan) = Fan::try_from(index) {
                    control
                        .set_fan_pwm(fan, pwm)
                        .map_err(|_| ThermalError::DeviceError.into())
                } else {
                    Err(ThermalError::InvalidFan.into())
                }
            }
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

    fn disable_watchdog(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<ThermalError>> {
        (self as &mut ServerImpl<B>)
            .set_watchdog(I2cWatchdog::Disabled)
            .map_err(Into::into)
    }

    fn enable_watchdog(
        &mut self,
        _: &RecvMessage,
        timeout_s: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        let wd = match timeout_s {
            5 => I2cWatchdog::FiveSeconds,
            10 => I2cWatchdog::TenSeconds,
            30 => I2cWatchdog::ThirtySeconds,
            _ => return Err(ThermalError::InvalidWatchdogTime.into()),
        };
        (self as &mut ServerImpl<B>)
            .set_watchdog(wd)
            .map_err(Into::into)
    }
}

impl<B: BspT> NotificationHandler for ServerImpl<'_, B> {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), TIMER_MASK);

        match &mut self.mode {
            ModeState::NotStarted(bsp) => {
                // See if we can leave the Off state yet.
                if bsp.power_mode() != 0 {
                    // We can!
                    let sensor_api = SensorApi::from(SENSOR.get_task_id());
                    let control = ThermalControl::new(*bsp, sensor_api);

                    self.mode = ModeState::Started {
                        control,
                        auto: false,
                        counter: 0,
                    };
                    self.set_mode_manual(PWMDuty(0)).unwrap();
                }
            }

            ModeState::Started {
                auto: true,
                control,
                counter,
            } => {
                *counter += 1;

                if *counter % CONTROL_RATE == 0 {
                    // TODO: what to do with errors here?
                    control.run_control();
                } else {
                    let _ = control.read_sensors();
                }
            }

            ModeState::Started {
                auto: false,
                control,
                counter,
            } => {
                // TODO do we need to advance the counter here?
                *counter += 1;

                // Ignore read errors, since the control loop isn't actually
                // running in this mode.
                let _ = control.read_sensors();
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();

    let bsp = Bsp::new(i2c_task);

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        mode: ModeState::NotStarted(&bsp),
        deadline,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::ThermalError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
