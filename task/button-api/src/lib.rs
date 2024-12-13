// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the 'button' task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};

#[derive(Copy, Clone, Debug, FromPrimitive, IdolError, counters::Count)]
pub enum ButtonError {
    InvalidValue = 1,
    TaskRestarted = 2,
}

impl From<idol_runtime::ServerDeath> for ButtonError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        ButtonError::TaskRestarted
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
