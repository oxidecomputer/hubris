// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};

#[derive(FromPrimitive, IdolError)]
#[repr(u32)]
pub enum UpdateError {
    BadLength = 1,
    UpdateInProgress = 2,
    OutOfBounds = 3,
    Timeout = 4,
    // Specific to STM32H7
    EccDoubleErr = 5,
    EccSingleErr = 6,
    SecureErr = 7,   // If we get this something has gone very wrong
    ReadProtErr = 8, // If we get this something has gone very wrong
    WriteEraseErr = 9,
    InconsistencyErr = 10,
    StrobeErr = 11,
    ProgSeqErr = 12,
    WriteProtErr = 13,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
