// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Sensor task.

#![no_std]

use userlib::*;

use derive_idol_err::IdolError;
use drv_i2c_api::ResponseCode;
use hubpack::SerializedSize;
use num_derive::FromPrimitive;
use serde::{Deserialize, Serialize};

/// A validated sensor ID.
///
/// `SensorId`s are used to reference an individual sensor in the [`Sensor`] IPC
/// interface.
///
/// This is, internally, a `u32` index which has been validated (by
/// [`SensorId::new()`], [`SensorId::try_new()`], or the [`TryFrom`]`<u32>`
/// implementation) to be less than or equal to the number of sensors in the
/// build-time configuration ([`config::NUM_SENSORS`]).
#[derive(
    zerocopy::IntoBytes,
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub struct SensorId(u32);

pub struct InvalidSensor;

impl SensorId {
    /// The maximum allowable `SensorId` value on this system.
    pub const MAX_VALUE: u32 = config::NUM_SENSORS as u32;

    /// Returns a new `SensorId` for the provided `u32` value.
    ///
    /// This must be a `const fn`, as it is used in `static` initializers.
    ///
    /// # Panics
    ///
    /// If `id` is greater than [`config::NUM_SENSORS`].
    #[must_use]
    pub const fn new(id: u32) -> Self {
        // NOTE: we `match` on the return value from `try_new` here, rather than
        // using `unwrap()` or `unwrap_lite()`, because this must be a `const
        // fn`, and those methods are not `const` (as `unwrap()` formats the
        // error, and `unwrap_lite()` is a trait method).
        match Self::try_new(id) {
            Ok(id) => id,
            Err(_) => panic!(),
        }
    }

    /// Returns a new `SensorId` for the provided `u32` value, if it is less
    /// than or equal to [`config::NUM_SENSORS`].
    pub const fn try_new(id: u32) -> Result<Self, InvalidSensor> {
        // On devboard targets without sensors, `NUM_SENSORS` is zero,
        // because...there are no sensors. Clippy will tell us we're being
        // "absurd" for checking if something is greater than or equal to zero,
        // but it has no understanding of the fact that the value depends on the
        // board config, so...who's *really* the absurd one here?
        #[allow(clippy::absurd_extreme_comparisons)]
        if id >= Self::MAX_VALUE {
            Err(InvalidSensor)
        } else {
            Ok(Self(id))
        }
    }

    /// Converts an array of `SensorId`s into an array of `u32`s.
    pub fn into_u32_array<const N: usize>(ids: [Self; N]) -> [u32; N] {
        ids.map(Into::into)
    }
}

impl TryFrom<u32> for SensorId {
    type Error = InvalidSensor;

    fn try_from(id: u32) -> Result<Self, Self::Error> {
        Self::try_new(id)
    }
}

impl From<SensorId> for u32 {
    fn from(id: SensorId) -> Self {
        id.0
    }
}

impl From<SensorId> for usize {
    fn from(id: SensorId) -> Self {
        id.0 as usize
    }
}

impl<'de> Deserialize<'de> for SensorId {
    fn deserialize<D>(deserializer: D) -> Result<SensorId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        u32::deserialize(deserializer)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("invalid sensor ID"))
    }
}

#[derive(Copy, Clone, Debug, SerializedSize, Serialize, Deserialize)]
pub struct Reading {
    pub timestamp: u64,
    pub value: f32,
}

impl Reading {
    pub fn new(value: f32, timestamp: u64) -> Self {
        Self { timestamp, value }
    }
}

//
// Note that [`counter_encoding`] relies on [`NoData`] being numbered from 0 and
// being numbered sequentially.
//
#[derive(
    zerocopy::IntoBytes,
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(u8)]
pub enum NoData {
    DeviceOff = 0,
    DeviceError = 1,
    DeviceNotPresent = 2,
    DeviceUnavailable = 3,
    DeviceTimeout = 4,
}

impl NoData {
    ///
    /// Routine to determine the number of bits and size of shift
    /// to pack a counter for each [`NoData`] variant into type `T`.
    ///
    pub fn counter_encoding<T>(self) -> (usize, usize) {
        //
        // We need to encode the number of variants in [`NoData`] here.  There
        // is a very convenient core::mem::variant_count() that does exactly
        // this, but it's currently unstable -- so instead we have an
        // exhaustive match to assure that the enum can't be updated without
        // modifying this code.
        //
        let nbits = (core::mem::size_of::<T>() * 8)
            / match self {
                NoData::DeviceOff
                | NoData::DeviceError
                | NoData::DeviceNotPresent
                | NoData::DeviceUnavailable
                | NoData::DeviceTimeout => 5,
            };

        let shift = (self as usize) * nbits;
        (nbits, shift)
    }
}

impl From<ResponseCode> for NoData {
    fn from(code: ResponseCode) -> NoData {
        match code {
            ResponseCode::NoDevice => NoData::DeviceNotPresent,
            ResponseCode::NoRegister => NoData::DeviceUnavailable,
            ResponseCode::BusLocked
            | ResponseCode::BusLockedMux
            | ResponseCode::ControllerBusy => NoData::DeviceTimeout,
            _ => NoData::DeviceError,
        }
    }
}

/// Flexible sensor error type, indicating either a caller or sensor error
///
/// This is effectively the [`NoData`] error with an added
/// [`SensorError::NoReading`] variant.
#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum SensorError {
    NoReading = 2,
    NotPresent = 3,
    DeviceError = 4,
    DeviceUnavailable = 5,
    DeviceTimeout = 6,
    DeviceOff = 7,
}

impl From<NoData> for SensorError {
    fn from(nodatum: NoData) -> SensorError {
        match nodatum {
            NoData::DeviceOff => SensorError::DeviceOff,
            NoData::DeviceNotPresent => SensorError::NotPresent,
            NoData::DeviceError => SensorError::DeviceError,
            NoData::DeviceUnavailable => SensorError::DeviceUnavailable,
            NoData::DeviceTimeout => SensorError::DeviceTimeout,
        }
    }
}

impl Sensor {
    /// Post the given data with a timestamp of now
    #[inline]
    pub fn post_now(&self, id: SensorId, value: f32) {
        self.post(id, value, sys_get_timer().now)
    }

    /// Post the given `NoData` error with a timestamp of now
    #[inline]
    pub fn nodata_now(&self, id: SensorId, nodata: NoData) {
        self.nodata(id, nodata, sys_get_timer().now)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/sensor_config.rs"));
