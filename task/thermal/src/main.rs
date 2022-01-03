// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

use drv_i2c_api::ResponseCode;
use drv_i2c_devices::max31790::*;
use drv_i2c_devices::sbtsi::*;
use drv_i2c_devices::tmp116::*;
use drv_i2c_devices::TempSensor;
use idol_runtime::{NotificationHandler, RequestError};
use task_sensor_api as sensor_api;
use task_thermal_api::ThermalError;
use userlib::units::*;
use userlib::*;

use sensor_api::{NoData, SensorId};

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

enum Zone {
    East,
    Central,
    West,
}

enum Device {
    North(Zone, Tmp116),
    South(Zone, Tmp116),
    CPU(SbTsi),
}

struct Sensor {
    device: Device,
    id: SensorId,
}

fn temp_read<E, T: TempSensor<E>>(device: &T) -> Result<Celsius, ThermalError>
where
    ResponseCode: From<E>,
{
    match device.read_temperature() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();

            let e = match err {
                ResponseCode::NoDevice => ThermalError::SensorNotPresent,
                ResponseCode::NoRegister => ThermalError::SensorUnavailable,
                ResponseCode::BusLocked
                | ResponseCode::BusLockedMux
                | ResponseCode::ControllerLocked => ThermalError::SensorTimeout,
                _ => ThermalError::SensorError,
            };

            Err(e)
        }
    }
}

impl Sensor {
    fn read_temp(&self) -> Result<Celsius, ThermalError> {
        match &self.device {
            Device::North(_, dev) | Device::South(_, dev) => temp_read(dev),
            Device::CPU(dev) => temp_read(dev),
        }
    }
}

fn read_fans(fctrl: &Max31790) {
    let mut ndx = 0;

    for fan in 0..MAX_FANS {
        let fan = Fan::new(fan).unwrap();

        match fctrl.fan_rpm(fan) {
            Ok(rval) if rval.0 != 0 => {
                sys_log!("{}: {}: RPM={}", fctrl, fan, rval.0);
            }
            Ok(_) => {}
            Err(err) => {
                sys_log!("{}: {}: failed: {:?}", fctrl, fan, err);
            }
        }

        ndx = ndx + 1;
    }
}

const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS
    + sensors::NUM_SBTSI_TEMPERATURE_SENSORS;

fn temperature_sensors() -> [Sensor; NUM_TEMPERATURE_SENSORS] {
    let task = I2C.get_task_id();

    [
        // North and south zones are inverted with respect to one another;
        // see Gimlet issue #1302 for details.
        Sensor {
            device: Device::North(
                Zone::East,
                Tmp116::new(&devices::tmp117_northeast(task)),
            ),
            id: sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::North(
                Zone::Central,
                Tmp116::new(&devices::tmp117_north(task)),
            ),
            id: sensors::TMP117_NORTH_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::North(
                Zone::West,
                Tmp116::new(&devices::tmp117_northwest(task)),
            ),
            id: sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::South(
                Zone::East,
                Tmp116::new(&devices::tmp117_southeast(task)),
            ),
            id: sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::South(
                Zone::Central,
                Tmp116::new(&devices::tmp117_south(task)),
            ),
            id: sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::South(
                Zone::West,
                Tmp116::new(&devices::tmp117_southwest(task)),
            ),
            id: sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
        },
        Sensor {
            device: Device::CPU(SbTsi::new(&devices::sbtsi(task)[0])),
            id: sensors::SBTSI_TEMPERATURE_SENSOR,
        },
    ]
}

struct ServerImpl {
    sensor: sensor_api::Sensor,
    sensors: [Sensor; NUM_TEMPERATURE_SENSORS],
    data: [Option<Result<f32, ThermalError>>; NUM_TEMPERATURE_SENSORS],
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl idl::InOrderThermalImpl for ServerImpl {
    fn read_sensor(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<f32, RequestError<ThermalError>> {
        if index < NUM_TEMPERATURE_SENSORS {
            match self.data[index] {
                Some(Err(err)) => Err(err.into()),
                Some(Ok(reading)) => Ok(reading),
                None => Err(ThermalError::NoReading.into()),
            }
        } else {
            Err(ThermalError::InvalidSensor.into())
        }
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), TIMER_MASK);

        for (index, sensor) in self.sensors.iter().enumerate() {
            self.data[index] = match sensor.read_temp() {
                Ok(reading) => {
                    self.sensor.post(sensor.id, reading.0).unwrap();
                    Some(Ok(reading.0))
                }
                Err(e) => {
                    self.sensor
                        .nodata(
                            sensor.id,
                            match e {
                                ThermalError::SensorNotPresent => {
                                    NoData::DeviceNotPresent
                                }
                                _ => NoData::DeviceError,
                            },
                        )
                        .unwrap();
                    Some(Err(e))
                }
            };
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let task = I2C.get_task_id();

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gimlet-1")] {
            let fctrl = Max31790::new(&devices::max31790(task)[0]);
        } else {
            compile_error!("unknown board");
        }
    }

    loop {
        match fctrl.initialize() {
            Ok(_) => {
                sys_log!("{}: initialization successful", fctrl);
                break;
            }
            Err(err) => {
                sys_log!("{}: initialization failed: {:?}", fctrl, err);
                hl::sleep_for(1000);
            }
        }
    }

    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        sensor: sensor_api::Sensor::from(SENSOR.get_task_id()),
        sensors: temperature_sensors(),
        data: [None; NUM_TEMPERATURE_SENSORS],
        deadline,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::ThermalError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
