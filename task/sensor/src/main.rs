// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sensor management

#![no_std]
#![no_main]

use idol_runtime::{NotificationHandler, RequestError};
use task_sensor_api::{NoData, Reading, SensorError, SensorId};
use userlib::*;

use task_sensor_api::config::NUM_SENSORS;

#[derive(Copy, Clone)]
enum LastReading {
    Data,
    Error,
}

struct ServerImpl {
    // We're using structure-of-arrays packing here because otherwise padding
    // eats up a considerable amount of RAM; for example, Sidecar goes from 2868
    // to 4200 bytes of RAM!
    //
    // The compiler is smart enough to present `None` with an invalid
    // `LastReading` variant tag, so we don't need to store presence separately.
    last_reading: &'static mut [Option<LastReading>; NUM_SENSORS],

    data_value: &'static mut [f32; NUM_SENSORS],
    data_time: &'static mut [u64; NUM_SENSORS],

    err_value: &'static mut [NoData; NUM_SENSORS],
    err_time: &'static mut [u64; NUM_SENSORS],

    nerrors: &'static mut [u32; NUM_SENSORS],
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl idl::InOrderSensorImpl for ServerImpl {
    fn get(
        &mut self,
        msg: &RecvMessage,
        id: SensorId,
    ) -> Result<f32, RequestError<SensorError>> {
        self.get_reading(msg, id).map(|r| r.value)
    }

    fn get_reading(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Reading, RequestError<SensorError>> {
        let index = id.0 as usize;

        if index < NUM_SENSORS {
            match self.last_reading[index] {
                None => Err(SensorError::NoReading.into()),
                Some(LastReading::Error) => {
                    let err: SensorError = self.err_value[index].into();
                    Err(err.into())
                }
                Some(LastReading::Data) => Ok(Reading::new(
                    self.data_value[index],
                    self.data_time[index],
                )),
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
        timestamp: u64,
    ) -> Result<(), RequestError<SensorError>> {
        let index = id.0 as usize;

        if index < NUM_SENSORS {
            self.last_reading[index] = Some(LastReading::Data);
            self.data_value[index] = value;
            self.data_time[index] = timestamp;
            Ok(())
        } else {
            Err(SensorError::InvalidSensor.into())
        }
    }

    fn nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        nodata: NoData,
        timestamp: u64,
    ) -> Result<(), RequestError<SensorError>> {
        let index = id.0 as usize;

        if index < NUM_SENSORS {
            self.last_reading[index] = Some(LastReading::Error);
            self.err_value[index] = nodata;
            self.err_time[index] = timestamp;

            //
            // We pack per-`NoData` counters into a u32.
            //
            let (nbits, shift) = nodata.counter_encoding::<u32>();
            let mask = (1 << nbits) - 1;
            let bitmask = mask << shift;
            let incr = 1 << shift;

            //
            // Perform a saturating increment by checking our current value
            // against our bitmask: if we have unset bits, we can safely add.
            //
            if self.nerrors[index] & bitmask != bitmask {
                self.nerrors[index] += incr;
            }

            Ok(())
        } else {
            Err(SensorError::InvalidSensor.into())
        }
    }

    fn get_nerrors(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<u32, RequestError<SensorError>> {
        let index = id.0 as usize;

        if index < NUM_SENSORS {
            Ok(self.nerrors[index])
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

    let (last_reading, data_value, data_time, err_value, err_time, nerrors) = mutable_statics::mutable_statics! {
        static mut LAST_READING: [Option<LastReading>; NUM_SENSORS] = [|| None; _];
        static mut DATA_VALUE: [f32; NUM_SENSORS] = [|| f32::NAN; _];
        static mut DATA_TIME: [u64; NUM_SENSORS] = [|| 0u64; _];
        static mut ERR_VALUE: [NoData; NUM_SENSORS] = [|| NoData::DeviceUnavailable; _];
        static mut ERR_TIME: [u64; NUM_SENSORS] = [|| 0; _];
        static mut NERRORS: [u32; NUM_SENSORS] = [|| 0; _];
    };

    let mut server = ServerImpl {
        last_reading,
        data_value,
        data_time,
        err_value,
        err_time,
        nerrors,
        deadline,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{NoData, Reading, SensorError, SensorId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
