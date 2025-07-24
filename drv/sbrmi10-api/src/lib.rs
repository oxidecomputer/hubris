// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SB-RMI driver

#![no_std]

use derive_idol_err::IdolError;
use drv_i2c_devices::sbrmi10;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, IdolError, counters::Count)]
pub enum Sbrmi10Error {
    Unavailable = 1,
    Unsupported,
    BusLocked,
    I2cError,
    BadThreadId,
    BadCpuidInput,
    CpuidError,
    CpuidUnavailable,
    CpuidTimeout,
    RdmsrError,
}

impl From<drv_i2c_api::ResponseCode> for Sbrmi10Error {
    fn from(code: drv_i2c_api::ResponseCode) -> Self {
        match code {
            drv_i2c_api::ResponseCode::NoDevice => Self::Unavailable,
            drv_i2c_api::ResponseCode::BusLocked => Self::BusLocked,
            _ => Self::I2cError,
        }
    }
}

impl From<sbrmi10::Error> for Sbrmi10Error {
    fn from(err: sbrmi10::Error) -> Self {
        use sbrmi10::{Error, StatusCode};

        match err {
            Error::BadRegisterRead { code, .. } => code.into(),
            Error::BadCpuidRead { code } => code.into(),
            Error::BadRdmsr { code, .. } => code.into(),
            Error::BadThreadId => Self::BadThreadId,
            Error::BadCpuidInput => Self::BadCpuidInput,
            Error::BadCpuidLength { .. } => Self::CpuidError,
            Error::CpuidFailed { code } => match code {
                StatusCode::CommandTimeout => Self::CpuidTimeout,
                StatusCode::UnsupportedCommand => Self::CpuidUnavailable,
                _ => Self::CpuidError,
            },
            Error::BadRdmsrLength { .. } => Self::RdmsrError,
            Error::RdmsrFailed { .. } => Self::RdmsrError,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
