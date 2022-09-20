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

#[derive(Copy, Clone, Debug)]
pub enum Reading {
    Absent,
    Value(f32),
    NoData(NoData),
}

#[derive(
    zerocopy::AsBytes, Copy, Clone, Debug, FromPrimitive, Eq, PartialEq,
)]
#[repr(u8)]
pub enum NoData {
    DeviceOff,
    DeviceError,
    DeviceNotPresent,
    DeviceUnavailable,
    DeviceTimeout,
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
