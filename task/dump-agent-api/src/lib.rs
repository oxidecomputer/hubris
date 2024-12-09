// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dump Agent task.

#![no_std]

use derive_idol_err::IdolError;
use dumper_api::DumperError;
use userlib::*;

pub use humpty::*;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum DumpAgentError {
    DumpAgentUnsupported = 1,
    InvalidArea,
    BadOffset,
    UnalignedOffset,
    UnalignedSegmentAddress,
    UnalignedSegmentLength,
    BadDumpResponse,
    NotSupported,
    DumpPresent,
    UnclaimedDumpArea,
    CannotClaimDumpArea,
    DumpAreaInUse,
    BadSegmentAdd,

    DumpMessageFailed,
    DumpFailed,
    DumpFailedSetup,
    DumpFailedRead,
    DumpFailedWrite,
    DumpFailedControl,
    DumpFailedUnknown,
    DumpFailedUnknownError,

    LeaseWriteFailed,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<DumperError> for DumpAgentError {
    fn from(err: DumperError) -> DumpAgentError {
        match err {
            DumperError::SetupFailed => DumpAgentError::DumpFailedSetup,
            DumperError::UnalignedAddress
            | DumperError::StartReadFailed
            | DumperError::ReadFailed
            | DumperError::BadDumpAreaHeader
            | DumperError::HeaderReadFailed => DumpAgentError::DumpFailedRead,
            DumperError::WriteFailed => DumpAgentError::DumpFailedWrite,
            DumperError::FailedToHalt
            | DumperError::FailedToResumeAfterFailure
            | DumperError::FailedToResume => DumpAgentError::DumpFailedControl,
            DumperError::DumpFailed => DumpAgentError::DumpFailed,
            _ => DumpAgentError::DumpFailedUnknown,
        }
    }
}

impl From<DumpAgentError> for humpty::udp::Error {
    fn from(d: DumpAgentError) -> Self {
        use humpty::udp::Error;
        match d {
            DumpAgentError::DumpAgentUnsupported => Error::DumpAgentUnsupported,
            DumpAgentError::InvalidArea => Error::InvalidArea,
            DumpAgentError::BadOffset => Error::BadOffset,
            DumpAgentError::UnalignedOffset => Error::UnalignedOffset,
            DumpAgentError::UnalignedSegmentAddress => {
                Error::UnalignedSegmentAddress
            }
            DumpAgentError::UnalignedSegmentLength => {
                Error::UnalignedSegmentLength
            }
            DumpAgentError::DumpFailed => Error::DumpFailed,
            DumpAgentError::NotSupported => Error::NotSupported,
            DumpAgentError::DumpPresent => Error::DumpPresent,
            DumpAgentError::UnclaimedDumpArea => Error::UnclaimedDumpArea,
            DumpAgentError::CannotClaimDumpArea => Error::CannotClaimDumpArea,
            DumpAgentError::DumpAreaInUse => Error::DumpAreaInUse,
            DumpAgentError::BadSegmentAdd => Error::BadSegmentAdd,
            DumpAgentError::ServerRestarted => Error::ServerRestarted,
            DumpAgentError::BadDumpResponse => Error::BadDumpResponse,
            DumpAgentError::DumpMessageFailed => Error::DumpMessageFailed,
            DumpAgentError::DumpFailedSetup => Error::DumpFailedSetup,
            DumpAgentError::DumpFailedRead => Error::DumpFailedRead,
            DumpAgentError::DumpFailedWrite => Error::DumpFailedWrite,
            DumpAgentError::DumpFailedControl => Error::DumpFailedControl,
            DumpAgentError::DumpFailedUnknown => Error::DumpFailedUnknown,
            DumpAgentError::DumpFailedUnknownError
            | DumpAgentError::LeaseWriteFailed => Error::DumpFailedUnknownError,
        }
    }
}

pub const DUMP_READ_SIZE: usize = 256;

//
// We use the version field to denote how a dump area is being used.
//
pub const DUMP_AGENT_VERSION: u8 = 0x10_u8;
pub const DUMP_AGENT_TASKS: u8 = 0x12_u8;
pub const DUMP_AGENT_SYSTEM: u8 = 0x13_u8;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
