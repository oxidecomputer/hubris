// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Sidecar

use crate::{
    bsp::BspT,
    control::{Device, FanControl, InputChannel, TemperatureSensor},
};
use core::convert::TryFrom;
use drv_sidecar_seq_api::Sequencer;
use drv_i2c_devices::tmp117::*;
use drv_i2c_devices::tmp451::*;
use drv_i2c_devices::max31790::Max31790;
use task_sensor_api::SensorId;
use userlib::{task_slot, units::Celsius, TaskId};
use ringbuf::*;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Initialize,
    Fan(usize),
    Fans,
    Controller,
}

ringbuf!(Trace, 32, Trace::None);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

task_slot!(SEQUENCER, sequencer);

const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;
// const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_TMP451_TEMPERATURE_SENSORS;
const NUM_TEMPERATURE_INPUTS: usize = 0;
const NUM_FANS: usize = sensors::NUM_MAX31790_SPEED_SENSORS;

pub(crate) struct Bsp {
    inputs: [InputChannel; NUM_TEMPERATURE_INPUTS],

    /// Monitored sensors
    misc_sensors: [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Fans and their respective RPM sensors
    fans: [(crate::Fan, SensorId); NUM_FANS],

    fctrl: FanControl,

    seq: Sequencer,
}

impl BspT for Bsp {
    fn inputs(&self) -> &[InputChannel] {
        &self.inputs
    }

    fn misc_sensors(&self) -> &[TemperatureSensor] {
        &self.misc_sensors
    }

    fn fans(&self) -> &[(crate::Fan, SensorId)] {
        &self.fans
    }

    fn fan_control(
        &self,
        fan: crate::Fan,
        mut fctrl: impl FnMut(
            &crate::control::FanControl,
            drv_i2c_devices::max31790::Fan,
        )
    ) {
        fctrl(&self.fctrl, fan.into())
    }

    fn fan_controls(
        &self,
        mut fctrl: impl FnMut(
            &crate::control::FanControl,
        )
    ) {
        fctrl(&self.fctrl)
    }

    fn power_mode(&self) -> u32 {
        // TODO: this needs to be done properly
        u32::MAX
    }

    fn new(i2c_task: TaskId) -> Self {
        ringbuf_entry!(Trace::Initialize);

        // Awkwardly build the fan array, because there's not a great way
        // to build a fixed-size array from a function
        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            ringbuf_entry!(Trace::Fan(i));
            *f = Some((
                crate::Fan(i as u8),
                sensors::MAX31790_SPEED_SENSORS[i],
            ));
        }
        let fans = fans.map(Option::unwrap);
        ringbuf_entry!(Trace::Fans);

        // Initializes and build a handle to the fan controller IC
        let fctrl = Max31790::new(&devices::max31790(i2c_task)[0]);
        fctrl.initialize().unwrap();

        ringbuf_entry!(Trace::Controller);

        // Handle for the sequencer task, which we check for power state
        let seq = Sequencer::from(SEQUENCER.get_task_id());

        Self {
            seq,
            fans,
            fctrl: FanControl::Max31790(fctrl),

            inputs: [],

            // We monitor and log all of the air temperatures
            misc_sensors: [
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_northeast(
                        i2c_task,
                    ))),
                    sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_nne(
                        i2c_task,
                    ))),
                    sensors::TMP117_NNE_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_nnw(
                        i2c_task,
                    ))),
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
