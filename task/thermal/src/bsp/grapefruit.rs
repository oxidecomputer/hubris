// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Medusa

use crate::control::{ChannelType, PidConfig};
use crate::control::{FanPresence, InputStatus};
use drv_i2c_devices::max31790::I2cWatchdog;
use task_sensor_api::SensorId;
use task_thermal_api::{ThermalError, ThermalProperties};
use userlib::TaskId;
use userlib::units::{Celsius, PWMDuty};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

// This BSP uses i2c temperature inputs
#[path = "./common/i2c_temp_input.rs"]
mod i2c_temp_input;
use i2c_temp_input::{
    Device, InputChannel, InputChannelMetadata, TemperatureSensor,
};

#[path = "./common/emc2305.rs"]
mod emc2305;
use emc2305::Emc2305State;

////////////////////////////////////////////////////////////////////////////////
// Constants!

// Temperature inputs (I2C devices), which are used in the control loop.
const NUM_TEMPERATURE_INPUTS: usize = 1;

// Number of individual fans
const NUM_FANS: usize = 4;

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
    pub inputs: &'static mut [InputChannel; NUM_TEMPERATURE_INPUTS],

    fans: &'static mut [Fan; NUM_FANS],
    fans_added: bool,
    i2c_task: TaskId,

    fctrl: Emc2305State,
}

impl crate::control::BspInterface for Bsp {
    //             // TODO: this is all made up, copied from tuned Gimlet values
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 35.0,
        gain_p: 1.75,
        gain_i: 0.0135,
        gain_d: 0.4,
        min_output: 15.0,
        max_output: 100.0,
    };

    fn power_down(&self) -> Result<(), crate::SeqError> {
        Ok(())
    }

    fn power_mode(&self) -> PowerBitmask {
        PowerBitmask::ON
    }

    fn read_fan_presence(
        &mut self,
    ) -> Result<
        impl Iterator<Item = crate::control::FanPresence>,
        crate::SeqError,
    > {
        let report_new = !self.fans_added;
        self.fans_added = true;
        Ok(self.fans.iter().map(move |f| FanPresence::Present {
            fan_id: f.bsp_data.into(),
            new: report_new,
        }))
    }

    fn read_fan_rpms(
        &mut self,
    ) -> impl Iterator<Item = crate::control::FanStatus> {
        self.fctrl.read_fan_rpms(self.fans)
    }

    fn read_misc_sensors(
        &self,
    ) -> impl Iterator<
        Item = (SensorId, Result<Celsius, task_thermal_api::SensorReadError>),
    > {
        core::iter::empty()
    }

    fn read_inputs(
        &mut self,
        mode: PowerBitmask,
    ) -> impl Iterator<Item = crate::control::InputReadingOutcome> {
        let task = &self.i2c_task;
        self.inputs
            .iter_mut()
            .map(move |i| i.do_reading(mode, task))
    }

    fn read_dynamic_inputs_back_from_sensor_api(
        &mut self,
        _sensor_api: &task_sensor_api::Sensor,
    ) {
        // No dynamic inputs
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
        self.inputs.iter().all(InputChannel::has_reading)
        // && self.dynamic_inputs...
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // All inputs MUST have a previous reading or this will panic, though the
    // readings may be allowed to be Missing if the model allows it. Dynamic
    // inputs that are not present will be skipped.
    fn all_present_inputs_status(
        &self,
    ) -> impl Iterator<Item = InputStatus<'_>> {
        self.inputs.iter().filter_map(|input| input.status())
        // .chain(self.dynamic_inputs...)
    }

    fn reset_all_values(&mut self) {
        self.inputs.iter_mut().for_each(|i| i.reset_value());
        // self.dynamic_inputs...
    }

    fn set_all_watchdogs(
        &mut self,
        watchdog: I2cWatchdog,
    ) -> Result<(), ThermalError> {
        // Only one watchdog to configure here!
        self.fctrl
            .try_initialize()?
            .set_watchdog(!matches!(watchdog, I2cWatchdog::Disabled))
            .map_err(|_| ThermalError::DeviceError)
    }

    // If a fan is missing, set PWMDuty(0). Attempt to apply to ALL fans,
    // even if some fail. return the LAST error if any.
    fn set_all_fan_duty(&mut self, duty: PWMDuty) -> Result<(), ThermalError> {
        let fctrl = self.fctrl.try_initialize()?;
        let mut any_err = false;

        // Note: DON'T short circuit here!
        for fan in self.fans.iter_mut() {
            any_err |= fctrl.set_pwm(fan.bsp_data, duty).is_err();
        }

        if any_err {
            Err(ThermalError::DeviceError)
        } else {
            Ok(())
        }
    }
}

impl Bsp {
    pub fn new(i2c_task: TaskId) -> Self {
        let fctrl =
            Emc2305State::new(&devices::emc2305(i2c_task)[0], NUM_FANS as u8);

        static INPUTS_ONCE: static_cell::ClaimOnceCell<
            [InputChannel; NUM_TEMPERATURE_INPUTS],
        > = static_cell::ClaimOnceCell::new(INPUTS);

        static FANS_ONCE: static_cell::ClaimOnceCell<[Fan; NUM_FANS]> =
            static_cell::ClaimOnceCell::new(FANS);

        Self {
            fans_added: false,
            fans: FANS_ONCE.claim(),

            inputs: INPUTS_ONCE.claim(),

            fctrl,
            i2c_task,
        }
    }
}

type Fan = crate::control::Fan<drv_i2c_devices::emc2305::Fan>;
const FANS: [Fan; NUM_FANS] = [
    Fan::new(
        sensors::EMC2305_SPEED_SENSORS[0],
        drv_i2c_devices::emc2305::Fan::new_const(0),
    ),
    Fan::new(
        sensors::EMC2305_SPEED_SENSORS[1],
        drv_i2c_devices::emc2305::Fan::new_const(1),
    ),
    Fan::new(
        sensors::EMC2305_SPEED_SENSORS[2],
        drv_i2c_devices::emc2305::Fan::new_const(2),
    ),
    Fan::new(
        sensors::EMC2305_SPEED_SENSORS[3],
        drv_i2c_devices::emc2305::Fan::new_const(3),
    ),
];

// This is completely made up!
const LM75_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(60f32),
    critical_temperature: Celsius(70f32),
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
