// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Gimlet rev B hardware
//!
//! This is identical to the rev A BSP except for the TMP451, which is in
//! a different power domain.

use crate::{
    bsp::BspT,
    control::{Device, FanControl, InputChannel, TemperatureSensor},
};
use core::convert::TryInto;
use drv_gimlet_seq_api::{PowerState, Sequencer};
use drv_i2c_devices::max31790::*;
use drv_i2c_devices::sbtsi::*;
use drv_i2c_devices::tmp117::*;
use drv_i2c_devices::tmp451::*;
use drv_i2c_devices::tse2004av::*;
use task_sensor_api::SensorId;
use userlib::{task_slot, units::Celsius, TaskId};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

task_slot!(SEQ, gimlet_seq);

const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;
const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_SBTSI_TEMPERATURE_SENSORS
    + sensors::NUM_TMP451_TEMPERATURE_SENSORS
    + sensors::NUM_TSE2004AV_TEMPERATURE_SENSORS;
const NUM_FANS: usize = drv_i2c_devices::max31790::MAX_FANS as usize;

pub(crate) struct Bsp {
    /// Controlled sensors
    inputs: [InputChannel; NUM_TEMPERATURE_INPUTS],

    /// Monitored sensors
    misc_sensors: [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Fan RPM sensors
    fans: [SensorId; NUM_FANS],

    fctrl: Max31790,

    seq: Sequencer,
}

// Use bitmasks to determine when sensors should be polled
const POWER_STATE_A2: u32 = 0b001;
const POWER_STATE_A0: u32 = 0b010;

impl BspT for Bsp {
    fn inputs(&self) -> &[InputChannel] {
        &self.inputs
    }

    fn misc_sensors(&self) -> &[TemperatureSensor] {
        &self.misc_sensors
    }

    fn fans(&self) -> &[SensorId] {
        &self.fans
    }

    fn fan_control(&self, fan: crate::Fan) -> FanControl<'_> {
        FanControl::Max31790(&self.fctrl, fan.0.try_into().unwrap())
    }

    fn for_each_fctrl(&self, mut fctrl: impl FnMut(FanControl<'_>)) {
        fctrl(self.fan_control(0.into()))
    }

    fn power_mode(&self) -> u32 {
        match self.seq.get_state() {
            Ok(p) => match p {
                PowerState::A0PlusHP | PowerState::A0 | PowerState::A1 => {
                    POWER_STATE_A0
                }
                PowerState::A2
                | PowerState::A2PlusMono
                | PowerState::A2PlusFans
                | PowerState::A0Thermtrip => POWER_STATE_A2,
            },
            // If `get_state` failed, then enable all sensors.  One of them
            // will presumably fail and will drop us into failsafe
            Err(_) => u32::MAX,
        }
    }

    fn new(i2c_task: TaskId) -> Self {
        // Awkwardly build the fan array, because there's not a great way
        // to build a fixed-size array from a function
        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            *f = Some(sensors::MAX31790_SPEED_SENSORS[i]);
        }

        let fans = fans.map(Option::unwrap);

        // Initializes and build a handle to the fan controller IC
        let fctrl = Max31790::new(&devices::max31790(i2c_task)[0]);
        fctrl.initialize().unwrap();

        // Handle for the sequencer task, which we check for power state
        let seq = Sequencer::from(SEQ.get_task_id());

        const MAX_DIMM_TEMP: Celsius = Celsius(60f32);
        const MAX_CPU_TEMP: Celsius = Celsius(60f32);
        const MAX_T6_TEMP: Celsius = Celsius(60f32);

        Self {
            seq,
            fans,
            fctrl,

            inputs: [
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::CPU(Sbtsi::new(&devices::sbtsi(i2c_task)[0])),
                        sensors::SBTSI_TEMPERATURE_SENSOR,
                    ),
                    MAX_CPU_TEMP,
                    POWER_STATE_A0,
                    false,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Tmp451(Tmp451::new(
                            &devices::tmp451(i2c_task)[0],
                            Target::Remote,
                        )),
                        sensors::TMP451_TEMPERATURE_SENSOR,
                    ),
                    MAX_T6_TEMP,
                    POWER_STATE_A0, // <-- this is different from rev A
                    false,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[0],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[0],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[1],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[1],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[2],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[2],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[3],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[3],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[4],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[4],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[5],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[5],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[6],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[6],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[7],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[7],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[8],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[8],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[9],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[9],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[10],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[10],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[11],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[11],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[12],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[12],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[13],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[13],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[14],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[14],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
                InputChannel::new(
                    TemperatureSensor::new(
                        Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[15],
                        )),
                        sensors::TSE2004AV_TEMPERATURE_SENSORS[15],
                    ),
                    MAX_DIMM_TEMP,
                    POWER_STATE_A0 | POWER_STATE_A2,
                    true,
                ),
            ],

            // We monitor and log all of the air temperatures
            //
            // North and south zones are inverted with respect to one
            // another on rev A; see Gimlet issue #1302 for details.
            misc_sensors: [
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_northeast(
                        i2c_task,
                    ))),
                    sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
                ),
                TemperatureSensor::new(
                    Device::Tmp117(Tmp117::new(&devices::tmp117_north(
                        i2c_task,
                    ))),
                    sensors::TMP117_NORTH_TEMPERATURE_SENSOR,
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
