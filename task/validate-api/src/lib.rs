// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Validate task.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_api::ResponseCode;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::AsBytes;

pub use drv_i2c_api::Mux;
pub use drv_i2c_api::Segment;
pub use task_sensor_api::SensorId;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum ValidateError {
    InvalidDevice = 1,
    BadValidation = 2,
    NotPresent = 3,
    DeviceError = 4,
    Unavailable = 5,
    DeviceTimeout = 6,
    DeviceOff = 7,
}

impl From<ResponseCode> for ValidateError {
    fn from(code: ResponseCode) -> ValidateError {
        match code {
            ResponseCode::NoDevice => ValidateError::NotPresent,
            ResponseCode::NoRegister => ValidateError::Unavailable,
            ResponseCode::BusLocked
            | ResponseCode::BusLockedMux
            | ResponseCode::ControllerLocked => ValidateError::DeviceTimeout,
            _ => ValidateError::DeviceError,
        }
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum ValidateOk {
    Present = 1,
    Validated = 2,
    Removed = 3,
}

#[derive(Copy, Clone, Debug, SerializedSize, Serialize, Deserialize)]
pub struct MuxSegment {
    pub mux: Mux,
    pub segment: Segment,
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
}

include!(concat!(env!("OUT_DIR"), "/device_descriptions.rs"));
