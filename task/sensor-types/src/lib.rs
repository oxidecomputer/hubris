// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client types for the Sensor API
#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_types::ResponseCode;
use hubpack::SerializedSize;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};

#[derive(
    zerocopy::AsBytes,
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(C)]
pub struct SensorId(pub u32);

impl From<u32> for SensorId {
    fn from(id: u32) -> Self {
        SensorId(id)
    }
}

impl From<SensorId> for u32 {
    fn from(id: SensorId) -> Self {
        id.0
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
    zerocopy::AsBytes,
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
/// This is effectively a union of [`SensorApiError`] and [`NoData`]
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum SensorError {
    InvalidSensor = 1,
    NoReading = 2,
    NotPresent = 3,
    DeviceError = 4,
    DeviceUnavailable = 5,
    DeviceTimeout = 6,
    DeviceOff = 7,
}

/// A non-device sensor error
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum SensorApiError {
    InvalidSensor = 1,
    NoReading = 2,
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

impl From<SensorApiError> for SensorError {
    fn from(e: SensorApiError) -> SensorError {
        match e {
            SensorApiError::InvalidSensor => SensorError::InvalidSensor,
            SensorApiError::NoReading => SensorError::NoReading,
        }
    }
}
