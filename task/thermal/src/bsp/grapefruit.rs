// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Medusa

use crate::control::{
    ChannelType, ControllerInitError, Device, Emc2305State, FanControl, Fans,
    InputChannel, PidConfig, TemperatureSensor,
};
use task_sensor_api::SensorId;
use task_thermal_api::ThermalProperties;
use userlib::units::Celsius;
use userlib::TaskId;
use userlib::UnwrapLite;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

////////////////////////////////////////////////////////////////////////////////
// Constants!

// Air temperature sensors, which aren't used in the control loop
const NUM_TEMPERATURE_SENSORS: usize = 0;

// Temperature inputs (I2C devices), which are used in the control loop.
pub const NUM_TEMPERATURE_INPUTS: usize = 1;

// External temperature inputs, which are provided to the task over IPC
pub const NUM_DYNAMIC_TEMPERATURE_INPUTS: usize = 0;

// Number of individual fans
pub const NUM_FANS: usize = 4;

// Run the PID loop on startup
pub const USE_CONTROLLER: bool = true;

////////////////////////////////////////////////////////////////////////////////

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct PowerBitmask: u32 {
        const ON = 0b00000001;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SeqError {}

#[allow(dead_code)]
pub(crate) struct Bsp {
    /// Controlled sensors
    pub inputs: &'static [InputChannel; NUM_TEMPERATURE_INPUTS],
    pub dynamic_inputs: &'static [SensorId; NUM_DYNAMIC_TEMPERATURE_INPUTS],

    /// Monitored sensors
    pub misc_sensors: &'static [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    pub pid_config: PidConfig,

    fctrl: Emc2305State,
}

impl Bsp {
    pub fn fan_control(
        &mut self,
        fan: crate::Fan,
    ) -> Result<crate::control::FanControl<'_>, ControllerInitError> {
        Ok(FanControl::Emc2305(
            self.fctrl.try_initialize()?,
            fan.0.try_into().unwrap_lite(),
        ))
    }

    pub fn for_each_fctrl(
        &mut self,
        fctrl: impl FnMut(FanControl<'_>),
    ) -> Result<(), ControllerInitError> {
        self.fan_control(0.into()).map(fctrl)
    }

    pub fn power_mode(&self) -> PowerBitmask {
        PowerBitmask::ON
    }

    pub fn power_down(&self) -> Result<(), SeqError> {
        Ok(())
    }

    pub fn get_fan_presence(&self) -> Result<Fans<{ NUM_FANS }>, SeqError> {
        let mut fans = Fans::new();
        for i in 0..NUM_FANS {
            fans[i] = Some(sensors::EMC2305_SPEED_SENSORS[i]);
        }
        Ok(fans)
    }

    pub fn fan_sensor_id(&self, i: usize) -> SensorId {
        sensors::EMC2305_SPEED_SENSORS[i]
    }

    pub fn new(i2c_task: TaskId) -> Self {
        let fctrl =
            Emc2305State::new(&devices::emc2305(i2c_task)[0], NUM_FANS as u8);

        Self {
            // TODO: this is all made up, copied from tuned Gimlet values
            pid_config: PidConfig {
                zero: 35.0,
                gain_p: 1.75,
                gain_i: 0.0135,
                gain_d: 0.4,
                min_output: 15.0,
                max_output: 100.0,
            },

            inputs: &INPUTS,
            dynamic_inputs: &[],
            misc_sensors: &MISC_SENSORS,

            fctrl,
        }
    }
}

// This is completely made up!
const LM75_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(60f32),
    critical_temperature: Celsius(70f32),
    power_down_temperature: Some(Celsius(80f32)),
    temperature_slew_deg_per_sec: 0.5,
    power_down_enabled: true,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [InputChannel::new(
    TemperatureSensor::new(
        Device::LM75,
        devices::pct2075_lm75_a,
        sensors::PCT2075_LM75_A_TEMPERATURE_SENSOR,
    ),
    LM75_THERMALS,
    PowerBitmask::ON,
    ChannelType::MustBePresent,
)];

const MISC_SENSORS: [TemperatureSensor; NUM_TEMPERATURE_SENSORS] = [];
