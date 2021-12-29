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
use task_thermal_api::ThermalError;
use userlib::units::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;

enum Zone {
    East,
    Central,
    West,
}

enum Sensor {
    North(Zone, Tmp116),
    South(Zone, Tmp116),
    CPU(SbTsi),
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
        match self {
            Sensor::North(_, dev) | Sensor::South(_, dev) => temp_read(dev),
            Sensor::CPU(dev) => temp_read(dev),
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

const NUM_SENSORS: usize = 7;

fn sensors() -> [Sensor; NUM_SENSORS] {
    let task = I2C.get_task_id();

    [
        Sensor::North(
            Zone::East,
            Tmp116::new(&devices::tmp117_rear_zone1(task)),
        ),
        Sensor::North(
            Zone::Central,
            Tmp116::new(&devices::tmp117_rear_zone2(task)),
        ),
        Sensor::North(
            Zone::West,
            Tmp116::new(&devices::tmp117_rear_zone3(task)),
        ),
        Sensor::South(
            Zone::East,
            Tmp116::new(&devices::tmp117_front_zone1(task)),
        ),
        Sensor::South(
            Zone::Central,
            Tmp116::new(&devices::tmp117_front_zone2(task)),
        ),
        Sensor::South(
            Zone::West,
            Tmp116::new(&devices::tmp117_front_zone3(task)),
        ),
        Sensor::CPU(SbTsi::new(&devices::sbtsi(task)[0])),
    ]
}

struct ServerImpl {
    sensors: [Sensor; NUM_SENSORS],
    data: [Option<Result<f32, ThermalError>>; NUM_SENSORS],
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
        if index < NUM_SENSORS {
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
                Ok(reading) => Some(Ok(reading.0)),
                Err(e) => Some(Err(e)),
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
        sensors: sensors(),
        data: [None; NUM_SENSORS],
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
