// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dumper task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
