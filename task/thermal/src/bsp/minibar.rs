// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Minibar

use crate::control::{
    ControllerInitError, FanControl, Fans, InputChannel, PidConfig,
    TemperatureSensor,
};
use task_sensor_api::SensorId;
use userlib::TaskId;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

////////////////////////////////////////////////////////////////////////////////
// Constants!

// Air temperature sensors, which aren't used in the control loop
const NUM_TEMPERATURE_SENSORS: usize = 0;

// Temperature inputs (I2C devices), which are used in the control loop.
pub const NUM_TEMPERATURE_INPUTS: usize = 0;

// External temperature inputs, which are provided to the task over IPC
// In practice, these are our transceivers.
pub const NUM_DYNAMIC_TEMPERATURE_INPUTS: usize = 0;

// Number of individual fans - Minibar has none!
pub const NUM_FANS: usize = 0;

// Run the PID loop on startup
pub const USE_CONTROLLER: bool = false;

////////////////////////////////////////////////////////////////////////////////

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct PowerBitmask: u32 {}
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SeqError {}

#[allow(dead_code)]
pub(crate) struct Bsp {
    pub inputs: &'static [InputChannel],
    pub dynamic_inputs: &'static [SensorId],

    /// Monitored sensors
    pub misc_sensors: &'static [TemperatureSensor],

    pub pid_config: PidConfig,
}

impl Bsp {
    pub fn fan_control(
        &self,
        _fan: crate::Fan,
    ) -> Result<crate::control::FanControl<'_>, ControllerInitError> {
        // Because we have zero fans, nothing should ever call fan_control.
        unreachable!()
    }

    pub fn for_each_fctrl(
        &self,
        mut _fctrl: impl FnMut(FanControl<'_>),
    ) -> Result<(), ControllerInitError> {
        // This one's reeeeal easy.
        Ok(())
    }

    pub fn power_mode(&self) -> PowerBitmask {
        PowerBitmask::empty()
    }

    pub fn power_down(&self) -> Result<(), SeqError> {
        Ok(())
    }

    pub fn get_fan_presence(&self) -> Result<Fans<{ NUM_FANS }>, SeqError> {
        Ok(Fans::new())
    }

    pub fn new(_i2c_task: TaskId) -> Self {
        Self {
            // PID config doesn't matter since we have no fans.
            pid_config: PidConfig {
                zero: 0.,
                gain_p: 0.,
                gain_i: 0.,
                gain_d: 0.,
                min_output: 0.,
                max_output: 100.,
            },

            inputs: &INPUTS,
            dynamic_inputs: &[],

            // We monitor and log all of the air temperatures
            misc_sensors: &MISC_SENSORS,
        }
    }
}

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [];

const MISC_SENSORS: [TemperatureSensor; NUM_TEMPERATURE_SENSORS] = [];
