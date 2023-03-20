// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Sidecar

use crate::control::{
    Device, FanControl, InputChannel, PidConfig, TemperatureSensor,
};
use core::convert::TryInto;
use drv_i2c_devices::max31790::Max31790;
use drv_i2c_devices::tmp451::*;
pub use drv_sidecar_seq_api::SeqError;
use drv_sidecar_seq_api::{Sequencer, TofinoSeqState, TofinoSequencerPolicy};
use task_sensor_api::SensorId;
use task_thermal_api::ThermalProperties;
use userlib::{task_slot, units::Celsius, TaskId};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

task_slot!(SEQUENCER, sequencer);

////////////////////////////////////////////////////////////////////////////////
// Constants!

// Air temperature sensors, which aren't used in the control loop
const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;

// Temperature inputs (I2C devices), which are used in the control loop.
pub const NUM_TEMPERATURE_INPUTS: usize =
    sensors::NUM_TMP451_TEMPERATURE_SENSORS;

// External temperature inputs, which are provided to the task over IPC
// In practice, these are our transceivers.
pub const NUM_DYNAMIC_TEMPERATURE_INPUTS: usize =
    drv_transceivers_api::NUM_PORTS as usize;

const NUM_FANS: usize = sensors::NUM_MAX31790_SPEED_SENSORS;

// Run the PID loop on startup
pub const USE_CONTROLLER: bool = true;

////////////////////////////////////////////////////////////////////////////////

bitflags::bitflags! {
    pub struct PowerBitmask: u32 {
        // As far as I know, we don't have any devices which are active only
        // in A2; you probably want to use `POWER_STATE_A0_OR_A2` instead
        const A2 = 0b00000001;
        const A0 = 0b00000010;
        const A0_OR_A2 = Self::A0.bits | Self::A2.bits;
    }
}

#[allow(dead_code)]
pub(crate) struct Bsp {
    pub inputs: &'static [InputChannel],
    pub dynamic_inputs: &'static [SensorId],

    /// Monitored sensors
    pub misc_sensors: &'static [TemperatureSensor],

    /// Fans and their respective RPM sensors
    pub fans: [SensorId; NUM_FANS],

    /// Our two fan controllers: east for 0/1 and west for 1/2
    fctrl_east: Max31790,
    fctrl_west: Max31790,

    seq: Sequencer,

    pub pid_config: PidConfig,
}

impl Bsp {
    pub fn fan_control(
        &self,
        fan: crate::Fan,
    ) -> crate::control::FanControl<'_> {
        //
        // Fan 0/1 are on the east max31790; fan 2/3 are on west max31790.  And
        // because each fan has in fact two fans, here is the mapping of
        // index to controller and fan:
        //
        // Index    Controller     Fan           MAX31790 Fan
        //     0    East           NNE           0
        //     1    East           SNE           1
        //     2    East           Northeast     2
        //     3    East           Southeast     3
        //     4    West           Northwest     0
        //     5    West           Southwest     1
        //     6    West           NNW           2
        //     7    West           SNW           3
        //
        if fan.0 < 4 {
            //
            // East side: straight mapping of fan index to MAX31790 fan
            //
            FanControl::Max31790(&self.fctrl_east, fan.0.try_into().unwrap())
        } else if fan.0 < 8 {
            //
            // West side: subtract 4 to get MAX31790 fan
            //
            FanControl::Max31790(
                &self.fctrl_west,
                (fan.0 - 4).try_into().unwrap(),
            )
        } else {
            //
            // Illegal fan
            //
            panic!();
        }
    }

    pub fn for_each_fctrl(&self, mut fctrl: impl FnMut(FanControl<'_>)) {
        // Run the function on each fan control chip
        fctrl(self.fan_control(0.into()));
        fctrl(self.fan_control(4.into()));
    }

    pub fn power_mode(&self) -> PowerBitmask {
        match self.seq.tofino_seq_state() {
            Ok(r) => match r {
                TofinoSeqState::A0 => PowerBitmask::A0,
                TofinoSeqState::Init
                | TofinoSeqState::A2
                | TofinoSeqState::InPowerUp
                | TofinoSeqState::InPowerDown => PowerBitmask::A2,
            },
            Err(_) => PowerBitmask::A0_OR_A2,
        }
    }

    pub fn power_down(&self) -> Result<(), SeqError> {
        self.seq
            .set_tofino_seq_policy(TofinoSequencerPolicy::Disabled)
    }

    pub fn new(i2c_task: TaskId) -> Self {
        // Awkwardly build the fan array, because there's not a great way
        // to build a fixed-size array from a function
        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            *f = Some(sensors::MAX31790_SPEED_SENSORS[i]);
        }
        let fans = fans.map(Option::unwrap);

        let fctrl_east = Max31790::new(&devices::max31790_east(i2c_task));
        let fctrl_west = Max31790::new(&devices::max31790_west(i2c_task));
        fctrl_east.initialize().unwrap();
        fctrl_west.initialize().unwrap();

        // Handle for the sequencer task, which we check for power state
        let seq = Sequencer::from(SEQUENCER.get_task_id());

        Self {
            seq,
            fans,
            fctrl_east,
            fctrl_west,

            // TODO: this is all made up, copied from tuned Gimlet values
            pid_config: PidConfig {
                zero: 35.0,
                gain_p: 1.75,
                gain_i: 0.0135,
                gain_d: 0.4,
            },

            inputs: &INPUTS,
            dynamic_inputs:
                &drv_transceivers_api::TRANSCEIVER_TEMPERATURE_SENSORS,

            // We monitor and log all of the air temperatures
            misc_sensors: &MISC_SENSORS,
        }
    }
}

//
// Guessing, big time
//
const TF2_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(60f32),
    critical_temperature: Celsius(70f32),
    power_down_temperature: Celsius(80f32),
    temperature_slew_deg_per_sec: 0.5,
};

// The VSC7448 has a maximum die temperature of 110°C, which is very
// hot.  Let's keep it a little cooler than that.
const VSC7448_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(85f32),
    critical_temperature: Celsius(95f32),
    power_down_temperature: Celsius(105f32),
    temperature_slew_deg_per_sec: 0.5,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [
    InputChannel::new(
        TemperatureSensor::new(
            Device::Tmp451(Target::Remote),
            devices::tmp451_tf2,
            sensors::TMP451_TF2_TEMPERATURE_SENSOR,
        ),
        TF2_THERMALS,
        PowerBitmask::A0,
        false,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Tmp451(Target::Remote),
            devices::tmp451_vsc7448,
            sensors::TMP451_VSC7448_TEMPERATURE_SENSOR,
        ),
        VSC7448_THERMALS,
        PowerBitmask::A0_OR_A2,
        false,
    ),
];

const MISC_SENSORS: [TemperatureSensor; NUM_TEMPERATURE_SENSORS] = [
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northeast,
        sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_nne,
        sensors::TMP117_NNE_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_nnw,
        sensors::TMP117_NNW_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northwest,
        sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southeast,
        sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_south,
        sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southwest,
        sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
    ),
];
