// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sensor management

#![no_std]
#![no_main]

use core::convert::Infallible;
use idol_runtime::{NotificationHandler, RequestError};
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
    fn get(&self, idx: SensorId) -> &T {
        unsafe {
            // Safety: Because a `SensorId` can only be constructed from a value less
            // than or equal to `NUM_SENSORS`, we can elide bounds checking it.
            self.0.get_unchecked(usize::from(idx))
        }
    }

    #[inline(always)]
    fn get_mut(&mut self, idx: SensorId) -> &mut T {
        unsafe {
            // Safety: Because a `SensorId` can only be constructed from a value less
            // than or equal to `NUM_SENSORS`, we can elide bounds checking it.
            self.0.get_unchecked_mut(usize::from(idx))
        }
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
}

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
        Ok(self.raw_reading(id))
    }

    fn get_last_data(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Option<(f32, u64)>, RequestError<Infallible>> {
        Ok(self.last_reading.get(id).and_then(|reading| match reading {
            LastReading::ErrorOnly => None,
            LastReading::Data | LastReading::DataOnly | LastReading::Error => {
                Some((*self.data_value.get(id), *self.data_time.get(id)))
            }
        }))
    }

    fn get_last_nodata(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<Option<(NoData, u64)>, RequestError<Infallible>> {
        Ok(self.last_reading.get(id).and_then(|reading| match reading {
            LastReading::DataOnly => None,
            LastReading::Data | LastReading::Error | LastReading::ErrorOnly => {
                Some((*self.err_value.get(id), *self.err_time.get(id)))
            }
        }))
    }

    fn get_min(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(f32, u64), RequestError<Infallible>> {
        Ok((*self.min_value.get(id), *self.min_time.get(id)))
    }

    fn get_max(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<(f32, u64), RequestError<Infallible>> {
        Ok((*self.max_value.get(id), *self.max_time.get(id)))
    }

    fn post(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
        value: f32,
        timestamp: u64,
    ) -> Result<(), RequestError<Infallible>> {
        let r = match self.last_reading.get(id) {
            None | Some(LastReading::DataOnly) => LastReading::DataOnly,
            Some(
                LastReading::Data | LastReading::Error | LastReading::ErrorOnly,
            ) => LastReading::Data,
        };

        *self.last_reading.get_mut(id) = Some(r);
        *self.data_value.get_mut(id) = value;
        *self.data_time.get_mut(id) = timestamp;

        let min_value = self.min_value.get_mut(id);
        if value < *min_value {
            *min_value = value;
            *self.min_time.get_mut(id) = timestamp;
        }

        let max_value = self.max_value.get_mut(id);
        if value > *max_value {
            *max_value = value;
            *self.max_time.get_mut(id) = timestamp;
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
        let r = match self.last_reading.get(id) {
            None | Some(LastReading::ErrorOnly) => LastReading::ErrorOnly,
            Some(
                LastReading::Data | LastReading::DataOnly | LastReading::Error,
            ) => LastReading::Error,
        };

        *self.last_reading.get_mut(id) = Some(r);
        *self.err_value.get_mut(id) = nodata;
        *self.err_time.get_mut(id) = timestamp;

        //
        // We pack per-`NoData` counters into a u32.
        //
        let (nbits, shift) = nodata.counter_encoding::<u32>();
        let mask = (1u32 << nbits).saturating_sub(1);
        let bitmask = mask << shift;
        let incr = 1 << shift;

        //
        // Perform a saturating increment by checking our current value
        // against our bitmask: if we have unset bits, we can safely add.
        //
        let nerrors = self.nerrors.get_mut(id);
        if *nerrors & bitmask != bitmask {
            *nerrors = nerrors.wrapping_add(incr);
        }

        Ok(())
    }

    fn get_nerrors(
        &mut self,
        _: &RecvMessage,
        id: SensorId,
    ) -> Result<u32, RequestError<Infallible>> {
        Ok(*self.nerrors.get_mut(id))
    }
}

impl ServerImpl {
    fn raw_reading(&self, id: SensorId) -> Option<(Result<f32, NoData>, u64)> {
        Some(match (*self.last_reading.get(id))? {
            LastReading::Data | LastReading::DataOnly => {
                (Ok(*self.data_value.get(id)), *self.data_time.get(id))
            }
            LastReading::Error | LastReading::ErrorOnly => {
                (Err(*self.err_value.get(id)), *self.err_time.get(id))
            }
        })
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    // N.B. if you are staring at this macro thinking that it looks like it
    // doesn't do anything and might be obsolescent, the key is the :upper. This
    // macro exists exclusively to uppercase the field names below.
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
    // Clippy doesn't like some of the IPC API's return types when nested inside
    // of a `Result<..., RequestError<Infallible>`. For now, let's allow the
    // type complexity lint here.
    // TODO(eliza): `idol`-generated code should probably always allow this lint?
    #![allow(clippy::type_complexity)]
    use super::{NoData, Reading, SensorError, SensorId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
