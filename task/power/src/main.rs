// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_gimlet_seq_api as seq_api;
use drv_i2c_devices::bmr491::*;
use drv_i2c_devices::raa229618::*;
use ringbuf::*;
use task_sensor_api as sensor_api;
use userlib::units::*;
use userlib::*;

use drv_i2c_api::ResponseCode;
use drv_i2c_devices::{CurrentSensor, TempSensor};

use sensor_api::{NoData, SensorId};
use seq_api::PowerState;

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);
task_slot!(SEQUENCER, gimlet_seq);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

use i2c_config::devices;
use i2c_config::sensors;

enum Device {
    IBC(Bmr491),
    Core(Raa229618),
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    State(seq_api::PowerState),
    Temperature(SensorId, Result<Celsius, ResponseCode>),
    None,
}

struct PowerController {
    state: seq_api::PowerState,
    device: Device,
    voltage: SensorId,
    current: SensorId,
    power: SensorId,
    temperature: Option<SensorId>,
}

ringbuf!(Trace, 16, Trace::None);

fn read_temperature<E, T: TempSensor<E>>(
    device: &T,
) -> Result<Celsius, ResponseCode>
where
    ResponseCode: From<E>,
{
    match device.read_temperature() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();
            Err(err)
        }
    }
}

fn read_current<E, T: CurrentSensor<E>>(
    device: &T,
) -> Result<Amperes, ResponseCode>
where
    ResponseCode: From<E>,
{
    match device.read_iout() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();
            Err(err)
        }
    }
}

impl PowerController {
    fn read_temperature(&self) -> Result<Celsius, ResponseCode> {
        match &self.device {
            Device::IBC(dev) => read_temperature(dev),
            Device::Core(dev) => read_temperature(dev),
        }
    }

    fn read_iout(&self) -> Result<Amperes, ResponseCode> {
        match &self.device {
            Device::IBC(dev) => read_current(dev),
            Device::Core(dev) => read_current(dev),
        }
    }
}

fn controllers() -> [PowerController; 2] {
    let task = I2C.get_task_id();

    [
        PowerController {
            state: seq_api::PowerState::A2,
            device: Device::IBC(Bmr491::new(&devices::bmr491(task)[0])),
            voltage: sensors::BMR491_VOLTAGE_SENSOR,
            current: sensors::BMR491_CURRENT_SENSOR,
            power: sensors::BMR491_POWER_SENSOR,
            temperature: Some(sensors::BMR491_TEMPERATURE_SENSOR),
        },
        PowerController {
            state: seq_api::PowerState::A0,
            device: Device::Core({
                let (device, rail) = i2c_config::pmbus::vdd_vcore(task);
                Raa229618::new(&device, rail)
            }),
            voltage: sensors::RAA229618_VDD_VCORE_VOLTAGE_SENSOR,
            current: sensors::RAA229618_VDD_VCORE_CURRENT_SENSOR,
            power: sensors::RAA229618_VDD_VCORE_POWER_SENSOR,
            temperature: Some(sensors::RAA229618_VDD_VCORE_TEMPERATURE_SENSOR),
        },
    ]
}

#[export_name = "main"]
fn main() -> ! {
    let sensor = sensor_api::Sensor::from(SENSOR.get_task_id());
    let sequencer = seq_api::Sequencer::from(SEQUENCER.get_task_id());

    let controllers = controllers();

    loop {
        hl::sleep_for(1000);

        let state = sequencer.get_state().unwrap();
        ringbuf_entry!(Trace::State(state));

        for controller in &controllers {
            if controller.state == PowerState::A0 && state != PowerState::A0 {
                sensor
                    .nodata(controller.voltage, NoData::DeviceOff)
                    .unwrap();
                sensor.nodata(controller.power, NoData::DeviceOff).unwrap();
                sensor
                    .nodata(controller.current, NoData::DeviceOff)
                    .unwrap();

                if let Some(id) = controller.temperature {
                    sensor.nodata(id, NoData::DeviceOff).unwrap();
                }

                continue;
            }

            if let Some(id) = controller.temperature {
                match controller.read_temperature() {
                    Ok(reading) => {
                        sensor.post(id, reading.0).unwrap();
                    }
                    Err(_) => {
                        sensor.nodata(id, NoData::DeviceError).unwrap();
                    }
                }
            }

            let id = controller.current;

            match controller.read_iout() {
                Ok(reading) => {
                    sensor.post(id, reading.0).unwrap();
                }
                Err(_) => {
                    sensor.nodata(id, NoData::DeviceError).unwrap();
                }
            }
        }
    }
}
