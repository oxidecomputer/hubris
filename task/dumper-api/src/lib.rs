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

///
/// These constitute an interface between the RoT and the SP in that the
/// error codes are interpreted by the dump agent and turned into dump agent
/// failures.  
///
/// These errors are also serialized and passed up to MGS, and as such they
/// should not be re-ordered. New errors may be added to the end, but if they
/// are they should be added to the `From<DumperError> for GwDumperError` below
/// as `GwDumper::Unknown` variants. `gateway-messages` should also be updated
/// to include the new variant so that in a second round of updates the from
/// can be changed to make the variant "known" again.
///
pub enum DumperError {
    SetupFailed = 1,
    UnalignedAddress = 2,
    StartReadFailed = 3,
    ReadFailed = 4,
    BadDumpAreaHeader = 5,
    WriteFailed = 6,
    HeaderReadFailed = 7,
    FailedToHalt = 8,
    FailedToResume = 9,
    FailedToResumeAfterFailure = 10,
    RegisterReadFailed = 11,

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
