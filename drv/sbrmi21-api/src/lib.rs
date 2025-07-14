// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SB-RMI driver

#![no_std]

use drv_i2c_api::ResponseCode;
use userlib::sys_send;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub enum StatusCode {
    Success,
    CommandTimeout,
    WarmReset,
    UnknownCommandFormat,
    InvalidReadLength,
    InvalidThread,
    UnsupportedCommand,
    CommandAborted,
    Unknown(u8),
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
    counters::Count,
)]
pub enum SbRmi21Error {
    Unavailable,
    Unsupported,
    BusLocked,
    CpuidUnavailable,
    CpuidTimeout,
    I2cError,
    BadRegisterRead {
        reg: u8,
        code: ResponseCode,
    },
    BadRegisterWrite {
        reg: u8,
        code: ResponseCode,
    },
    BadRegisterBlockWrite {
        reg: [u8; 2],
        len: u8,
        code: ResponseCode,
    },
    BadRegisterBlockRead {
        reg: [u8; 2],
        len: u8,
        code: ResponseCode,
    },
    BadThreadId {
        thread: u32,
    },
    BadCpuidInput,
    BadCpuidLength {
        length: u8,
    },
    BadCpuidRead {
        code: ResponseCode,
    },
    CpuidFailed {
        code: StatusCode,
    },
    BadRdmsrLength {
        length: u8,
    },
    BadRdmsr {
        code: ResponseCode,
    },
    RdmsrFailed {
        code: StatusCode,
    },
    BadMailboxCmd,
    MailboxResponseMismatch {
        wanted: u8,
        got: u8,
    },
    MailboxCmdFailed {
        code: SbRmi21MailboxErrorCode,
    },
}

impl From<SbRmi21Error> for ResponseCode {
    fn from(err: SbRmi21Error) -> Self {
        match err {
            SbRmi21Error::BadRegisterRead { code, .. } => code,
            SbRmi21Error::BadRegisterWrite { code, .. } => code,
            SbRmi21Error::BadRegisterBlockWrite { code, .. } => code,
            SbRmi21Error::BadCpuidRead { code } => code,
            SbRmi21Error::BadRdmsr { code } => code,
            _ => ResponseCode::BadResponse,
        }
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub enum SbRmi21MailboxErrorCode {
    Success,
    CommandAborted,
    UnknownCommand,
    InvalidCore,
    CommandFailedWithError(u32),
    InvalidInputArguments,
    InvalidOobRasConfig,
    DataNotReady,
    UnknownError(u8),
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
