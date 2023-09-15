// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sensor management

#![no_std]
#![no_main]

use idol_runtime::{NotificationHandler, RequestError};
use task_sensor_api::{NoData, Reading, SensorApiError, SensorError, SensorId};
use userlib::*;

use task_sensor_api::config::NUM_SENSORS;

#[derive(Copy, Clone)]
enum LastReading {
    /// We have only seen a data reading
    DataOnly,
    /// We have only seen an error reading
    ErrorOnly,
    /// The most recent reading is a data reading, but we have seen both
    Data,
    /// The most recent reading is an error reading, but we have seen both
    Error,
}

/// Zero-cost array wrapper that can be indexed with a `SensorId`
struct SensorArray<T: 'static>(&'static mut [T; NUM_SENSORS]);
impl<T: 'static> core::ops::Index<SensorId> for SensorArray<T> {
    type Output = T;
    #[inline(always)]
    fn index(&self, idx: SensorId) -> &Self::Output {
        &self.0[idx.0 as usize]
    }
}
impl<T: 'static> core::ops::IndexMut<SensorId> for SensorArray<T> {
    #[inline(always)]
    fn index_mut(&mut self, idx: SensorId) -> &mut Self::Output {
        &mut self.0[idx.0 as usize]
    }
}
impl<T: 'static> SensorArray<T> {
    #[inline(always)]
    fn get(&self, idx: SensorId) -> Option<&T> {
        self.0.get(idx.0 as usize)
    }
}

struct ServerImpl {
    // We're using structure-of-arrays packing here because otherwise padding
    // eats up a considerable amount of RAM; for example, Sidecar goes from 2868
    // to 4200 bytes of RAM!
    //
    // The compiler is smart enough to present `None` with an invalid
    // `LastReading` variant tag, so we don't need to store presence separately.
    last_reading: SensorArray<Option<LastReading>>,

    data_value: SensorArray<f32>,
    data_time: SensorArray<u64>,

    err_value: SensorArray<NoData>,
    err_time: SensorArray<u64>,

    nerrors: SensorArray<u32>,
    deadline: u64,
}

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
        msg: &RecvMessage,
        id: SensorId,
    ) -> Result<Reading, RequestError<SensorError>> {
        self.get_raw_reading(msg, id)
            .map_err(|e| e.map_runtime(SensorError::from))
            .and_then(|(value, timestamp)| match value {
                Ok(value) => Ok(Reading { value, timestamp }),
                Err(e) => Err(SensorError::from(e).into()),
            })
    }

    fn get_raw_reading(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(Result<f32, NoData>, u64), RequestError<SensorApiError>> {
        match self.last_reading(id)? {
            Some(LastReading::Data | LastReading::DataOnly) => {
                Ok((Ok(self.data_value[id]), self.data_time[id]))
            }
            Some(LastReading::Error | LastReading::ErrorOnly) => {
                Ok((Err(self.err_value[id]), self.err_time[id]))
            }
            None => Err(SensorApiError::NoReading.into()),
        }
    }

    fn get_last_data(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(f32, u64), RequestError<SensorApiError>> {
        match self.last_reading(id)? {
            None | Some(LastReading::ErrorOnly) => {
                Err(SensorApiError::NoReading.into())
            }
            Some(
                LastReading::Data | LastReading::DataOnly | LastReading::Error,
            ) => Ok((self.data_value[id], self.data_time[id])),
        }
    }

    fn get_last_nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(NoData, u64), RequestError<SensorApiError>> {
        match self.last_reading(id)? {
            None | Some(LastReading::DataOnly) => {
                Err(SensorApiError::NoReading.into())
            }
            Some(
                LastReading::Data | LastReading::Error | LastReading::ErrorOnly,
            ) => Ok((self.err_value[id], self.err_time[id])),
        }
    }

    fn post(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        value: f32,
        timestamp: u64,
    ) -> Result<(), RequestError<SensorApiError>> {
        let r = match self.last_reading(id)? {
            None | Some(LastReading::DataOnly) => LastReading::DataOnly,
            Some(
                LastReading::Data | LastReading::Error | LastReading::ErrorOnly,
            ) => LastReading::Data,
        };

        self.last_reading[id] = Some(r);
        self.data_value[id] = value;
        self.data_time[id] = timestamp;
        Ok(())
    }

    fn nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        nodata: NoData,
        timestamp: u64,
    ) -> Result<(), RequestError<SensorApiError>> {
        let r = match self.last_reading(id)? {
            None | Some(LastReading::ErrorOnly) => LastReading::ErrorOnly,
            Some(
                LastReading::Data | LastReading::DataOnly | LastReading::Error,
            ) => LastReading::Error,
        };

        self.last_reading[id] = Some(r);
        self.err_value[id] = nodata;
        self.err_time[id] = timestamp;

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
        if self.nerrors[id] & bitmask != bitmask {
            self.nerrors[id] += incr;
        }

        Ok(())
    }

    fn get_nerrors(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<u32, RequestError<SensorApiError>> {
        self.nerrors
            .get(id)
            .cloned()
            .ok_or_else(|| SensorApiError::InvalidSensor.into())
    }
}

impl ServerImpl {
    #[inline(always)]
    fn last_reading(
        &self,
        id: SensorId,
    ) -> Result<Option<LastReading>, SensorApiError> {
        self.last_reading
            .get(id)
            .cloned()
            .ok_or(SensorApiError::InvalidSensor)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    let (last_reading, data_value, data_time, err_value, err_time, nerrors) = mutable_statics::mutable_statics! {
        static mut LAST_READING: [Option<LastReading>; NUM_SENSORS] = [|| None; _];
        static mut DATA_VALUE: [f32; NUM_SENSORS] = [|| f32::NAN; _];
        static mut DATA_TIME: [u64; NUM_SENSORS] = [|| 0u64; _];
        static mut ERR_VALUE: [NoData; NUM_SENSORS] = [|| NoData::DeviceUnavailable; _];
        static mut ERR_TIME: [u64; NUM_SENSORS] = [|| 0; _];
        static mut NERRORS: [u32; NUM_SENSORS] = [|| 0; _];
    };

    let mut server = ServerImpl {
        last_reading: SensorArray(last_reading),
        data_value: SensorArray(data_value),
        data_time: SensorArray(data_time),
        err_value: SensorArray(err_value),
        err_time: SensorArray(err_time),
        nerrors: SensorArray(nerrors),
        deadline,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{NoData, Reading, SensorApiError, SensorError, SensorId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
