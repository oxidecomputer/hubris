// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    control::InputChannel, Device, FanControl, TemperatureSensor, Zone,
};
use drv_i2c_devices::max31790::*;
use drv_i2c_devices::sbtsi::*;
use drv_i2c_devices::tmp117::*;
use drv_i2c_devices::tmp451::*;
use drv_i2c_devices::tse2004av::*;
use task_sensor_api::SensorId;
use userlib::{units::Celsius, TaskId};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;
const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_SBTSI_TEMPERATURE_SENSORS
    + sensors::NUM_TMP451_TEMPERATURE_SENSORS
    + sensors::NUM_TSE2004AV_TEMPERATURE_SENSORS;
const NUM_FANS: usize = drv_i2c_devices::max31790::MAX_FANS as usize;

pub(crate) struct Bsp {
    pub inputs: [InputChannel; NUM_TEMPERATURE_INPUTS],
    pub misc_sensors: [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Handle to the fan control IC
    pub fctrl: FanControl,
    /// Array of fans
    pub fans: [(Fan, SensorId); NUM_FANS],
}

impl Bsp {
    pub fn new(i2c_task: TaskId) -> Self {
        // Initialize fan controller IC
        let fctrl = Max31790::new(&devices::max31790(i2c_task)[0]);
        fctrl.initialize().unwrap();

        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            *f = Some((Fan::from(i as u8), sensors::MAX31790_SPEED_SENSORS[i]));
        }
        let fans = fans.map(Option::unwrap);

        const MAX_DIMM_TEMP: Celsius = Celsius(80f32);

        Self {
            // The only input used for temperature is the CPU temperature
            inputs: [
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::CPU(Sbtsi::new(
                            &devices::sbtsi(i2c_task)[0],
                        )),
                        id: sensors::SBTSI_TEMPERATURE_SENSOR,
                    },
                    max_temp: Celsius(80f32),
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::T6Nic(Tmp451::new(
                            &devices::tmp451(i2c_task)[0],
                            Target::Remote,
                        )),
                        id: sensors::TMP451_TEMPERATURE_SENSOR,
                    },
                    max_temp: Celsius(80f32),
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[0],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[0],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[1],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[1],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[2],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[2],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[3],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[3],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[4],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[4],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[5],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[5],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[6],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[6],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[7],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[7],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[8],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[8],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[9],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[9],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[10],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[10],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[11],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[11],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[12],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[12],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[13],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[13],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[14],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[14],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
                InputChannel {
                    sensor: TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[15],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[15],
                    },
                    max_temp: MAX_DIMM_TEMP,
                },
            ],

            // We monitor and log all of the air temperatures
            misc_sensors: [
                // North and south zones are inverted with respect to one
                // another; see Gimlet issue #1302 for details.
                TemperatureSensor {
                    device: Device::North(
                        Zone::East,
                        Tmp117::new(&devices::tmp117_northeast(i2c_task)),
                    ),
                    id: sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::North(
                        Zone::Central,
                        Tmp117::new(&devices::tmp117_north(i2c_task)),
                    ),
                    id: sensors::TMP117_NORTH_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::North(
                        Zone::West,
                        Tmp117::new(&devices::tmp117_northwest(i2c_task)),
                    ),
                    id: sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::South(
                        Zone::East,
                        Tmp117::new(&devices::tmp117_southeast(i2c_task)),
                    ),
                    id: sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::South(
                        Zone::Central,
                        Tmp117::new(&devices::tmp117_south(i2c_task)),
                    ),
                    id: sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::South(
                        Zone::West,
                        Tmp117::new(&devices::tmp117_southwest(i2c_task)),
                    ),
                    id: sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
                },
            ],

            fctrl: FanControl::Max31790(fctrl),
            fans,
        }
    }
}
