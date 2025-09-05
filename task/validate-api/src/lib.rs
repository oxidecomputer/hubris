// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Validate task.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_api::ResponseCode;
use userlib::*;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

pub use task_sensor_api::SensorId;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum ValidateError {
    InvalidDevice = 1,
    BadValidation,
    NotPresent,
    DeviceError,
    Unavailable,
    DeviceTimeout,
    DeviceOff,
}

impl From<ResponseCode> for ValidateError {
    fn from(code: ResponseCode) -> ValidateError {
        match code {
            ResponseCode::NoDevice => ValidateError::NotPresent,
            ResponseCode::NoRegister => ValidateError::Unavailable,
            ResponseCode::BusLocked
            | ResponseCode::BusLockedMux
            | ResponseCode::ControllerBusy => ValidateError::DeviceTimeout,
            _ => ValidateError::DeviceError,
        }
    }
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u8)]
pub enum ValidateOk {
    Present = 1,
    Validated = 2,
    Removed = 3,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sensor {
    Temperature,
    Power,
    Current,
    Voltage,
    InputCurrent,
    InputVoltage,
    Speed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SensorDescription {
    pub name: Option<&'static str>,
    pub kind: Sensor,
    pub id: SensorId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceDescription {
    pub device: &'static str,
    pub description: &'static str,
    pub sensors: &'static [SensorDescription],
    pub id: [u8; MAX_ID_LENGTH],
    pub vpd: Option<VpdDescription>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VpdDescription {
    VpdTask(u8),
}

include!(concat!(env!("OUT_DIR"), "/device_descriptions.rs"));
