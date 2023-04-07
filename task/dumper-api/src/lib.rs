// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dumper task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

///
/// These constitute an interface between the RoT and the SP in that the
/// error codes are interpreted by the dump agent and turned into dump agent
/// failures.  Ultimately, these error conditions are merely informative,
/// but it is important that a particular error condition not be falsely
/// reported; error conditions can be added to or eliminated from this enum,
/// but the values for extant conditions should not be altered.
///
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
