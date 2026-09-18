// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for running the thermal loop on the host, under simulation.
//!
//! The smallest board that exercises the control loop: one LM75-class
//! temperature input, served by the fake I2C driver, and no fans. Fan duty
//! is accepted and discarded, so the loop runs its PID controller against
//! the simulated temperature without anything to drive.

use crate::control::{ActiveInputState, MiscSensorPollingOutcome};
use crate::control::{ChannelType, PidConfig};
use drv_i2c_devices::max31790::I2cWatchdog;
use task_sensor_api::SensorId;
use task_thermal_api::{ThermalError, ThermalProperties};
use userlib::TaskId;
use userlib::units::{Celsius, PWMDuty};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

#[path = "./common/i2c_temp_input.rs"]
mod i2c_temp_input;
use i2c_temp_input::{
    Device, InputChannel, InputChannelMetadata, TemperatureSensor,
};

const NUM_TEMPERATURE_INPUTS: usize = 1;
const NUM_FANS: usize = 0;

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct PowerBitmask: u32 {
        const ON = 0b00000001;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SeqError {}

pub(crate) struct Bsp {
    pub inputs: &'static mut [InputChannel; NUM_TEMPERATURE_INPUTS],
    fans: &'static mut [Fan; NUM_FANS],
    i2c_task: TaskId,
}

impl crate::control::BspInterface for Bsp {
    const USE_CONTROLLER: bool = true;

    // Borrowed from Grapefruit; nothing here is tuned.
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 35.0,
        gain_p: 1.75,
        gain_i: 0.0135,
        gain_d: 0.4,
        min_output: 15.0,
        max_output: 100.0,
    };

    type FanBspId = ();

    fn power_down(&self) -> Result<(), crate::SeqError> {
        Ok(())
    }

    fn power_mode(&self) -> PowerBitmask {
        PowerBitmask::ON
    }

    fn poll_fan_rpms(&mut self) -> impl Iterator<Item = &'_ mut Fan> {
        self.fans.iter_mut()
    }

    fn poll_misc_sensors(
        &self,
    ) -> impl Iterator<Item = MiscSensorPollingOutcome> {
        core::iter::empty()
    }

    fn poll_inputs(
        &mut self,
        mode: PowerBitmask,
    ) -> impl Iterator<Item = crate::control::InputPollingOutcome> {
        let task = &self.i2c_task;
        self.inputs
            .iter_mut()
            .map(move |i| i.poll_input(mode, task))
    }

    fn poll_dynamic_inputs(&mut self, _sensor_api: &task_sensor_api::Sensor) {}

    fn register_dynamic_input(
        &mut self,
        _index: usize,
        _model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        Err(ThermalError::InvalidIndex)
    }

    fn remove_dynamic_input(
        &mut self,
        _index: usize,
    ) -> Result<SensorId, ThermalError> {
        Err(ThermalError::InvalidIndex)
    }

    fn all_inputs_queried(&self) -> bool {
        self.inputs.iter().all(InputChannel::has_been_queried)
    }

    fn all_active_inputs(&self) -> impl Iterator<Item = ActiveInputState<'_>> {
        self.inputs.iter().filter_map(|input| input.active_state())
    }

    fn reset_all_values(&mut self) {
        let power = self.power_mode();
        self.inputs.iter_mut().for_each(|i| i.reset_value(power));
    }

    fn set_all_watchdogs(
        &mut self,
        _watchdog: I2cWatchdog,
    ) -> Result<(), ThermalError> {
        Ok(())
    }

    fn set_all_fan_duty(&mut self, _duty: PWMDuty) -> Result<(), ThermalError> {
        Ok(())
    }
}

impl Bsp {
    pub fn new(i2c_task: TaskId) -> Self {
        static INPUTS_ONCE: static_cell::ClaimOnceCell<
            [InputChannel; NUM_TEMPERATURE_INPUTS],
        > = static_cell::ClaimOnceCell::new(INPUTS);

        static FANS_ONCE: static_cell::ClaimOnceCell<[Fan; NUM_FANS]> =
            static_cell::ClaimOnceCell::new([]);

        Self {
            fans: FANS_ONCE.claim(),
            inputs: INPUTS_ONCE.claim(),
            i2c_task,
        }
    }
}

type Fan = crate::control::Fan<()>;

const LM75_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(35f32),
    critical_temperature: Celsius(60f32),
    power_down_temperature: Celsius(80f32),
    temperature_slew_deg_per_sec: 0.5,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] =
    [InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::LM75,
            devices::pct2075_lm75_a,
            sensors::PCT2075_LM75_A_TEMPERATURE_SENSOR,
        ),
        LM75_THERMALS,
        PowerBitmask::ON,
        ChannelType::MustBePresent,
    ))];
