// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    bsp::{BspData, BspT},
    control::InputChannel,
    Device, FanControl, TemperatureSensor,
};
use core::convert::TryFrom;
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
    /// Controlled sensors
    pub inputs: [InputChannel; NUM_TEMPERATURE_INPUTS],

    /// Monitored sensors
    pub misc_sensors: [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Fans and their respective RPM sensors
    pub fans: [(Fan, SensorId); NUM_FANS],

    i2c_task: TaskId,
}

impl BspT for Bsp {
    fn data(&mut self) -> BspData {
        // Initializes and build a handle to the fan controller IC
        let fctrl = Max31790::new(&devices::max31790(self.i2c_task)[0]);
        fctrl.initialize().unwrap();

        BspData {
            inputs: &mut self.inputs,
            misc_sensors: &mut self.misc_sensors,
            fans: &self.fans,
            fctrl: FanControl::Max31790(fctrl),
        }
    }

    fn new(i2c_task: TaskId) -> Self {
        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            *f = Some((
                Fan::try_from(i as u8).unwrap(),
                sensors::MAX31790_SPEED_SENSORS[i],
            ));
        }
        let fans = fans.map(Option::unwrap);

        const MAX_DIMM_TEMP: Celsius = Celsius(80f32);

        Self {
            // The only inputs used for temperature are the CPU and NIC
            // temperatures (right now)
            inputs: [
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::CPU(Sbtsi::new(
                            &devices::sbtsi(i2c_task)[0],
                        )),
                        id: sensors::SBTSI_TEMPERATURE_SENSOR,
                    },
                    Celsius(80f32),
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::T6Nic(Tmp451::new(
                            &devices::tmp451(i2c_task)[0],
                            Target::Remote,
                        )),
                        id: sensors::TMP451_TEMPERATURE_SENSOR,
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[0],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[0],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[1],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[1],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[2],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[2],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[3],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[3],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[4],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[4],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[5],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[5],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[6],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[6],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[7],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[7],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[8],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[8],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[9],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[9],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[10],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[10],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[11],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[11],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[12],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[12],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[13],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[13],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[14],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[14],
                    },
                    MAX_DIMM_TEMP,
                ),
                InputChannel::new(
                    TemperatureSensor {
                        device: Device::Dimm(Tse2004Av::new(
                            &devices::tse2004av(i2c_task)[15],
                        )),
                        id: sensors::TSE2004AV_TEMPERATURE_SENSORS[15],
                    },
                    MAX_DIMM_TEMP,
                ),
            ],

            // We monitor and log all of the air temperatures
            misc_sensors: [
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_northeast(i2c_task),
                    )),
                    id: sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_north(i2c_task),
                    )),
                    id: sensors::TMP117_NORTH_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_northwest(i2c_task),
                    )),
                    id: sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_southeast(i2c_task),
                    )),
                    id: sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_south(i2c_task),
                    )),
                    id: sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
                },
                TemperatureSensor {
                    device: Device::Tmp117(Tmp117::new(
                        &devices::tmp117_southwest(i2c_task),
                    )),
                    id: sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
                },
            ],

            fans,
            i2c_task,
        }
    }
}
