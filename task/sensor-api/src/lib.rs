// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Sensor task.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_api::ResponseCode;
use userlib::*;

#[derive(zerocopy::AsBytes, Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SensorId(pub usize);

impl From<usize> for SensorId {
    fn from(id: usize) -> Self {
        SensorId(id)
    }
}

impl From<SensorId> for usize {
    fn from(id: SensorId) -> Self {
        id.0
    }
}

#[derive(Copy, Clone, Debug, zerocopy::FromBytes, zerocopy::AsBytes)]
#[repr(C)]
pub struct Reading {
    pub timestamp: u64,
    pub value: f32,
    _pad: u32, // Required for FromBytes / AsBytes
}

impl Reading {
    pub fn new(value: f32, timestamp: u64) -> Self {
        Self {
            timestamp,
            value,
            _pad: 0,
        }
    }
}

//
// Note that [`counter_encoding`] relies on [`NoData`] being numbered from 0 and
// being numbered sequentially.
//
#[derive(
    zerocopy::AsBytes, Copy, Clone, Debug, FromPrimitive, Eq, PartialEq,
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
            | ResponseCode::ControllerLocked => NoData::DeviceTimeout,
            _ => NoData::DeviceError,
        }
    }
}

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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/sensor_config.rs"));
