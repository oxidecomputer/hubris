// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Validate task.

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_api::{I2cDevice, ResponseCode};
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

#[derive(Copy, Clone, Debug)]
pub struct DeviceDescription {
    pub device: &'static str,
    pub description: &'static str,
    pub sensors: &'static [SensorDescription],
    pub id: [u8; MAX_ID_LENGTH],
    #[cfg(feature = "fruid")]
    pub fruid: Option<FruidMode>,
}

#[cfg(feature = "fruid")]
#[derive(Copy, Clone)]
pub enum FruidMode {
    At24Csw080Barcode(fn(TaskId) -> I2cDevice),
    At24Csw080Nested(fn(TaskId) -> I2cDevice),
    Tmp117(fn(TaskId) -> I2cDevice),
}

#[cfg(feature = "fruid")]
impl core::fmt::Debug for FruidMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FruidMode::At24Csw080Barcode(_) => write!(f, "At24Csw080Barcode"),
            FruidMode::At24Csw080Nested(_) => write!(f, "At24Csw080Nested"),
            FruidMode::Tmp117(_) => write!(f, "Tmp117"),
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/device_descriptions.rs"));

#[cfg(feature = "fruid")]
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
