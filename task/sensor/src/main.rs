// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sensor management

#![no_std]
#![no_main]

use idol_runtime::{NotificationHandler, RequestError};
use task_sensor_api::{SensorError, SensorId, SensorReading};
use userlib::units::*;
use userlib::*;

// This is only included to determine the number of sensors
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

use i2c_config::sensors;
use sensors::NUM_SENSORS;

struct ServerImpl {
    data: [SensorReading; NUM_SENSORS],
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl idl::InOrderSensorImpl for ServerImpl {
    fn get(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<f32, RequestError<SensorError>> {
        let index = id.0;

        if index < NUM_SENSORS {
            match self.data[index] {
                SensorReading::NotPresent => {
                    Err(SensorError::NotPresent.into())
                }
                SensorReading::DeviceError => {
                    Err(SensorError::DeviceError.into())
                }
                SensorReading::Value(reading) => Ok(reading),
                SensorReading::None => Err(SensorError::NoReading.into()),
            }
        } else {
            Err(SensorError::InvalidSensor.into())
        }
    }

    fn post(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        value: f32,
    ) -> Result<(), RequestError<SensorError>> {
        let index = id.0;

        if index < NUM_SENSORS {
            self.data[index] = SensorReading::Value(value);
            Ok(())
        } else {
            Err(SensorError::InvalidSensor.into())
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
    }
}

#[export_name = "main"]
fn main() -> ! {
    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl {
        data: [SensorReading::None; NUM_SENSORS],
        deadline,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{SensorError, SensorId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
