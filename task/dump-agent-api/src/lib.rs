// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dump Agent task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

pub use humpty::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum DumpAgentError {
    DumpAgentUnsupported = 1,
    InvalidArea,
    BadOffset,
    UnalignedOffset,
    UnalignedSegmentAddress,
    UnalignedSegmentLength,
    DumpFailed,
    NotSupported,
    NoDumpAreas,
    DumpPresent,
    UnclaimedDumpArea,
    CannotClaimDumpArea,
    DumpAreaInUse,
    BadSegmentAdd,

    #[idol(server_death)]
    ServerRestarted,
}

pub const DUMP_READ_SIZE: usize = 256;

//
// We use the version field to denote how a dump area is being used.
//
pub const DUMP_AGENT_VERSION: u8 = 0x10_u8;
pub const DUMP_AGENT_TASKS: u8 = 0x12_u8;
pub const DUMP_AGENT_SYSTEM: u8 = 0x13_u8;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
