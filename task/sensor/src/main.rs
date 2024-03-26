// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sensor management

#![no_std]
#![no_main]

use core::convert::Infallible;
use idol_runtime::{ClientError, NotificationHandler, RequestError};
use task_sensor_api::{NoData, Reading, SensorError, SensorId};
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

impl<T: 'static> SensorArray<T> {
    #[inline(always)]
    fn get(&self, idx: SensorId) -> Result<&T, RequestError<Infallible>> {
        // Return `BadMessageContents` if the sensor ID is out of range.
        self.0
            .get(usize::from(idx))
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    #[inline(always)]
    fn get_mut(
        &mut self,
        idx: SensorId,
    ) -> Result<&mut T, RequestError<Infallible>> {
        // Return `BadMessageContents` if the sensor ID is out of range.
        self.0
            .get_mut(usize::from(idx))
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
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

    min_value: SensorArray<f32>,
    min_time: SensorArray<u64>,

    max_value: SensorArray<f32>,
    max_time: SensorArray<u64>,

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
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Reading, RequestError<SensorError>> {
        let (reading, timestamp) = self
            .raw_reading(id)
            .map_err(|e| match e {
                RequestError::Fail(e) => RequestError::Fail(e),
                RequestError::Runtime(e) => match e {},
            })?
            .ok_or(RequestError::Runtime(SensorError::NoReading))?;
        let value = reading.map_err(SensorError::from)?;
        Ok(Reading { value, timestamp })
    }

    fn get_raw_reading(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Option<(Result<f32, NoData>, u64)>, RequestError<Infallible>>
    {
        Ok(self.raw_reading(id)?)
    }

    fn get_last_data(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Option<(f32, u64)>, RequestError<Infallible>> {
        Ok(match self.last_reading(id)? {
            None | Some(LastReading::ErrorOnly) => None,
            Some(
                LastReading::Data | LastReading::DataOnly | LastReading::Error,
            ) => Some((*self.data_value.get(id)?, *self.data_time.get(id)?)),
        })
    }

    fn get_last_nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Option<(NoData, u64)>, RequestError<Infallible>> {
        Ok(match self.last_reading(id)? {
            Some(LastReading::DataOnly) | None => None,
            Some(
                LastReading::Data | LastReading::Error | LastReading::ErrorOnly,
            ) => Some((*self.err_value.get(id)?, *self.err_time.get(id)?)),
        })
    }

    fn get_min(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(f32, u64), RequestError<Infallible>> {
        Ok((*self.min_value.get(id)?, *self.min_time.get(id)?))
    }

    fn get_max(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(f32, u64), RequestError<Infallible>> {
        Ok((*self.max_value.get(id)?, *self.max_time.get(id)?))
    }

    fn post(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        value: f32,
        timestamp: u64,
    ) -> Result<(), RequestError<Infallible>> {
        let r = match self.last_reading(id)? {
            None | Some(LastReading::DataOnly) => LastReading::DataOnly,
            Some(
                LastReading::Data | LastReading::Error | LastReading::ErrorOnly,
            ) => LastReading::Data,
        };

        *self.last_reading.get_mut(id)? = Some(r);
        *self.data_value.get_mut(id)? = value;
        *self.data_time.get_mut(id)? = timestamp;

        let min_value = self.min_value.get_mut(id)?;
        if value < *min_value {
            *min_value = value;
            *self.min_time.get_mut(id)? = timestamp;
        }

        let max_value = self.max_value.get_mut(id)?;
        if value > *max_value {
            *max_value = value;
            *self.max_time.get_mut(id)? = timestamp;
        }

        Ok(())
    }

    fn nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        nodata: NoData,
        timestamp: u64,
    ) -> Result<(), RequestError<Infallible>> {
        let r = match self.last_reading(id)? {
            None | Some(LastReading::ErrorOnly) => LastReading::ErrorOnly,
            Some(
                LastReading::Data | LastReading::DataOnly | LastReading::Error,
            ) => LastReading::Error,
        };

        *self.last_reading.get_mut(id)? = Some(r);
        *self.err_value.get_mut(id)? = nodata;
        *self.err_time.get_mut(id)? = timestamp;

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
        let nerrors = self.nerrors.get_mut(id)?;
        if *nerrors & bitmask != bitmask {
            *nerrors += incr;
        }

        Ok(())
    }

    fn get_nerrors(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<u32, RequestError<Infallible>> {
        Ok(*self.nerrors.get_mut(id)?)
    }
}

impl ServerImpl {
    fn raw_reading(
        &self,
        id: SensorId,
    ) -> Result<Option<(Result<f32, NoData>, u64)>, RequestError<Infallible>>
    {
        Ok(match self.last_reading(id)? {
            None => None,
            Some(LastReading::Data | LastReading::DataOnly) => {
                Some((Ok(*self.data_value.get(id)?), *self.data_time.get(id)?))
            }
            Some(LastReading::Error | LastReading::ErrorOnly) => {
                Some((Err(*self.err_value.get(id)?), *self.err_time.get(id)?))
            }
        })
    }

    #[inline(always)]
    fn last_reading(
        &self,
        id: SensorId,
    ) -> Result<Option<LastReading>, RequestError<Infallible>> {
        Ok(self.last_reading.get(id)?.clone())
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

    macro_rules! declare_server {
        ($($name:ident: $t:ty = $n:expr;)*) => {{
            paste::paste! {
                let ($($name),*) = mutable_statics::mutable_statics! {
                    $(
                    static mut [<$name:upper>]: [$t; NUM_SENSORS] = [|| $n; _];
                    )*
                };
                let ($($name),*) = ($(SensorArray($name)),*);
                ServerImpl {
                    deadline,
                    $($name),*
                }
            }}
        };
    }

    let mut server = declare_server!(
        last_reading: Option<LastReading> = None;
        data_value: f32 = f32::NAN;
        data_time: u64 = 0u64;
        min_value: f32 = f32::MAX;
        min_time: u64 = 0u64;
        max_value: f32 = f32::MIN;
        max_time: u64 = 0u64;
        err_value: NoData = NoData::DeviceUnavailable;
        err_time: u64 = 0;
        nerrors: u32 = 0;
    );

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{NoData, Reading, SensorError, SensorId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
