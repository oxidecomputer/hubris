// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Sidecar

use crate::control::{
    Device, FanControl, InputChannel, PidConfig, TemperatureSensor,
    ThermalProperties,
};
use core::convert::TryInto;
use drv_i2c_devices::max31790::Max31790;
use drv_i2c_devices::tmp117::*;
use drv_i2c_devices::tmp451::*;
pub use drv_sidecar_seq_api::SeqError;
use drv_sidecar_seq_api::{Sequencer, TofinoSequencerPolicy};
use task_sensor_api::SensorId;
use userlib::{task_slot, units::Celsius, TaskId};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

task_slot!(SEQUENCER, sequencer);

const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;
pub const NUM_TEMPERATURE_INPUTS: usize =
    sensors::NUM_TMP451_TEMPERATURE_SENSORS;
const NUM_FANS: usize = sensors::NUM_MAX31790_SPEED_SENSORS;

// The Sidecar controller hasn't been tuned yet, so boot into manual mode
pub const USE_CONTROLLER: bool = false;

#[allow(dead_code)]
pub(crate) struct Bsp {
    pub inputs: [InputChannel; NUM_TEMPERATURE_INPUTS],

    /// Monitored sensors
    pub misc_sensors: [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

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

    pub fn power_mode(&self) -> u32 {
        // TODO: this needs to be done properly
        u32::MAX
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

        Self {
            seq,
            fans,
            fctrl_east,
            fctrl_west,

            // TODO: this is all made up
            pid_config: PidConfig {
                // If we're > 10 degrees from the target temperature, fans
                // should be on at full power.
                gain_p: 10.0,
                gain_i: 0.0,
                gain_d: 0.0,
            },

            inputs: [
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Tmp451(Tmp451::new(
                            &devices::tmp451_tf2(i2c_task),
                            Target::Remote,
                        )),
                        sensors::TMP451_TF2_TEMPERATURE_SENSOR,
                    ),
                    TF2_THERMALS,
                    0,
                    false,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Tmp451(Tmp451::new(
                            &devices::tmp451_vsc7448(i2c_task),
                            Target::Remote,
                        )),
                        sensors::TMP451_VSC7448_TEMPERATURE_SENSOR,
                    ),
                    VSC7448_THERMALS,
                    0,
                    false,
                ),
            ],

            // We monitor and log all of the air temperatures
            misc_sensors: [
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_northeast(
                        i2c_task,
                    ))),
                    sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_nne(i2c_task))),
                    sensors::TMP117_NNE_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_nnw(i2c_task))),
                    sensors::TMP117_NNW_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_northwest(
                        i2c_task,
                    ))),
                    sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_southeast(
                        i2c_task,
                    ))),
                    sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_south(
                        i2c_task,
                    ))),
                    sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_southwest(
                        i2c_task,
                    ))),
                    sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
                ),
            ],
        }
    }
}
