// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Minibar

use crate::control::PidConfig;
use task_sensor_api::SensorId;
use task_thermal_api::{ThermalError, ThermalProperties};
use userlib::TaskId;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

////////////////////////////////////////////////////////////////////////////////
// Constants!

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
pub(crate) struct Bsp {}

impl crate::control::BspInterface for Bsp {
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 0.,
        gain_p: 0.,
        gain_i: 0.,
        gain_d: 0.,
        min_output: 0.,
        max_output: 100.,
    };

    fn power_down(&self) -> Result<(), crate::SeqError> {
        Ok(())
    }

    fn power_mode(&self) -> PowerBitmask {
        PowerBitmask::empty()
    }

    fn read_fan_presence(
        &mut self,
    ) -> Result<
        impl Iterator<Item = crate::control::FanPresence>,
        crate::SeqError,
    > {
        Ok(core::iter::empty())
    }

    fn read_fan_rpms(
        &mut self,
    ) -> impl Iterator<Item = crate::control::FanReading> {
        core::iter::empty()
    }

    fn read_misc_sensors(
        &self,
    ) -> impl Iterator<
        Item = (
            SensorId,
            Result<userlib::units::Celsius, task_thermal_api::SensorReadError>,
        ),
    > {
        core::iter::empty()
    }

    fn read_inputs(
        &mut self,
        _mode: PowerBitmask,
    ) -> impl Iterator<Item = crate::control::InputReadingOutcome> {
        core::iter::empty()
    }

    fn read_dynamic_inputs_back_from_sensor_api(
        &mut self,
        _sensor_api: &task_sensor_api::Sensor,
    ) {
        // no dynamic inputs
    }

    // returns Ok(true) if this was a new input
    fn update_dynamic_input(
        &mut self,
        _index: usize,
        _model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    // sets last_reading to Some(Missing), returns sensor id
    fn remove_dynamic_input(
        &mut self,
        _index: usize,
    ) -> Result<SensorId, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    fn all_inputs_present(&self) -> bool {
        true
    }

    fn all_present_inputs_status(
        &self,
    ) -> impl Iterator<Item = crate::control::InputStatus<'_>> {
        core::iter::empty()
    }

    fn reset_all_values(&mut self) {
        // nothing to reset!
    }

    fn set_all_watchdogs(
        &mut self,
        _watchdog: drv_i2c_devices::max31790::I2cWatchdog,
    ) -> Result<(), task_thermal_api::ThermalError> {
        Ok(())
    }

    fn set_all_fan_duty(
        &mut self,
        _duty: userlib::units::PWMDuty,
    ) -> Result<(), task_thermal_api::ThermalError> {
        Ok(())
    }
}

impl Bsp {
    pub fn new(_i2c_task: TaskId) -> Self {
        Self {}
    }
}
