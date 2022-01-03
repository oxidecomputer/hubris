// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Sensor task.

#![no_std]

use userlib::*;

#[derive(zerocopy::AsBytes, Copy, Clone, Debug, PartialEq)]
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
    None,
    Value(f32),
    NoData(NoData),
}

#[derive(zerocopy::AsBytes, Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum NoData {
    DeviceOff,
    DeviceError,
    DeviceNotPresent,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
pub enum SensorError {
    InvalidSensor = 1,
    Unknown = 2,
    NotPresent = 3,
    DeviceError = 4,
    NoReading = 5,
}

impl From<SensorError> for u16 {
    fn from(rc: SensorError) -> Self {
        rc as u16
    }
}

impl From<SensorError> for u32 {
    fn from(rc: SensorError) -> Self {
        rc as u32
    }
}

impl core::convert::TryFrom<u32> for SensorError {
    type Error = ();
    fn try_from(rc: u32) -> Result<Self, Self::Error> {
        Self::from_u32(rc).ok_or(())
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
