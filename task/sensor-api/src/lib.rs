// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Sensor task.

#![no_std]

use userlib::*;

use core::convert::TryFrom;

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
pub use task_sensor_types::{NoData, Reading, SensorError};

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
    zerocopy::AsBytes,
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

impl Sensor {
    /// Post the given data with a timestamp of now
    #[inline]
    pub fn post_now(
        &self,
        id: SensorId,
        value: f32,
    ) -> Result<(), SensorApiError> {
        self.post(id, value, sys_get_timer().now)
    }

    /// Post the given `NoData` error with a timestamp of now
    #[inline]
    pub fn nodata_now(
        &self,
        id: SensorId,
        nodata: NoData,
    ) -> Result<(), SensorApiError> {
        self.nodata(id, nodata, sys_get_timer().now)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/sensor_config.rs"));
