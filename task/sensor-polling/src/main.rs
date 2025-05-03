// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Temperature and fan monitoring loop, for systems without thermal control

#![no_std]
#![no_main]

use drv_i2c_devices::mwocp68::{Error as Mwocp68Error, Mwocp68};
use ringbuf::*;
use task_sensor_api::{Sensor, SensorId};
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

/// Type containing all of our temperature sensor types, so we can store them
/// generically in an array.  Right now, we only support the MWOCP68.
#[allow(dead_code, clippy::upper_case_acronyms)]
pub enum Device {
    Mwocp68,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Mwocp68Error(Mwocp68Error),
}

impl From<Error> for task_sensor_api::NoData {
    fn from(e: Error) -> Self {
        match e {
            Error::Mwocp68Error(e) => match e {
                Mwocp68Error::BadRead { code, .. }
                | Mwocp68Error::BadWrite { code, .. }
                | Mwocp68Error::BadValidation { code, .. } => code.into(),
                Mwocp68Error::BadData { .. }
                | Mwocp68Error::InvalidData { .. } => Self::DeviceError,
                _ => Self::DeviceError,
            },
        }
    }
}

/// Represents a sensor in the system.
///
/// The sensor includes a device type, used to decide how to read it;
/// a free function that returns the raw `I2cDevice`, so that this can be
/// `const`); and the sensor ID, to post data to the `sensors` task.
pub struct TemperatureSensor {
    device: Device,
    builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
    temperature_sensors: &'static [SensorId],
    speed_sensors: &'static [SensorId],
}

impl TemperatureSensor {
    pub const fn new(
        device: Device,
        builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
        temperature_sensors: &'static [SensorId],
        speed_sensors: &'static [SensorId],
    ) -> Self {
        Self {
            device,
            builder,
            temperature_sensors,
            speed_sensors,
        }
    }

    fn poll(&self, i2c_task: TaskId, sensor_api: &Sensor) {
        let dev = (self.builder)(i2c_task);
        match &self.device {
            Device::Mwocp68 => {
                for (i, &s) in self.temperature_sensors.iter().enumerate() {
                    let m = Mwocp68::new(&dev, i.try_into().unwrap());
                    match m.read_temperature() {
                        Ok(v) => sensor_api.post_now(s, v.0),
                        Err(e) => {
                            let e = Error::Mwocp68Error(e);
                            ringbuf_entry!(Trace::TemperatureReadFailed(s, e));
                            sensor_api.nodata_now(s, e.into())
                        }
                    }
                }
                for (i, &s) in self.speed_sensors.iter().enumerate() {
                    let m = Mwocp68::new(&dev, i.try_into().unwrap());
                    match m.read_speed() {
                        Ok(v) => sensor_api.post_now(s, v.0),
                        Err(e) => {
                            let e = Error::Mwocp68Error(e);
                            ringbuf_entry!(Trace::SpeedReadFailed(s, e));
                            sensor_api.nodata_now(s, e.into())
                        }
                    }
                }
            }
        };
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    Start,
    SpeedReadFailed(SensorId, Error),
    TemperatureReadFailed(SensorId, Error),
}
ringbuf!(Trace, 32, Trace::None);

////////////////////////////////////////////////////////////////////////////////

const TIMER_INTERVAL: u64 = 1000;

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let sensor_api = Sensor::from(SENSOR.get_task_id());

    ringbuf_entry!(Trace::Start);

    loop {
        hl::sleep_for(TIMER_INTERVAL);
        for s in &SENSORS {
            s.poll(i2c_task, &sensor_api);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

use i2c_config::{devices, sensors};

#[cfg(any(target_board = "psc-b", target_board = "psc-c"))]
static SENSORS: [TemperatureSensor; 6] = [
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu0mcu,
        &sensors::MWOCP68_PSU0MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU0MCU_SPEED_SENSORS,
    ),
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu1mcu,
        &sensors::MWOCP68_PSU1MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU1MCU_SPEED_SENSORS,
    ),
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu2mcu,
        &sensors::MWOCP68_PSU2MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU2MCU_SPEED_SENSORS,
    ),
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu3mcu,
        &sensors::MWOCP68_PSU3MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU3MCU_SPEED_SENSORS,
    ),
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu4mcu,
        &sensors::MWOCP68_PSU4MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU4MCU_SPEED_SENSORS,
    ),
    TemperatureSensor::new(
        Device::Mwocp68,
        devices::mwocp68_psu5mcu,
        &sensors::MWOCP68_PSU5MCU_TEMPERATURE_SENSORS,
        &sensors::MWOCP68_PSU5MCU_SPEED_SENSORS,
    ),
];

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
