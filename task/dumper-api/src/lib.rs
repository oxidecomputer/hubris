// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dumper task.

#![no_std]

use derive_idol_err::IdolError;
use gateway_messages::DumperError as GwDumperError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IdolError,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum DumperError {
    SetupFailed = 1,
    UnalignedAddress,
    StartReadFailed,
    ReadFailed,
    BadDumpAreaHeader,
    WriteFailed,
    HeaderReadFailed,
    FailedToHalt,
    FailedToResume,
    FailedToResumeAfterFailure,
    RegisterReadFailed,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<DumperError> for GwDumperError {
    fn from(value: DumperError) -> Self {
        match value {
            DumperError::SetupFailed => Self::SetupFailed,
            DumperError::UnalignedAddress => Self::UnalignedAddress,
            DumperError::StartReadFailed => Self::StartReadFailed,
            DumperError::ReadFailed => Self::ReadFailed,
            DumperError::BadDumpAreaHeader => Self::BadDumpAreaHeader,
            DumperError::WriteFailed => Self::WriteFailed,
            DumperError::HeaderReadFailed => Self::HeaderReadFailed,
            DumperError::FailedToHalt => Self::FailedToHalt,
            DumperError::FailedToResume => Self::FailedToResume,
            DumperError::FailedToResumeAfterFailure => {
                Self::FailedToResumeAfterFailure
            }
            DumperError::RegisterReadFailed => Self::RegisterReadFailed,
            DumperError::ServerRestarted => Self::ServerRestarted,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
