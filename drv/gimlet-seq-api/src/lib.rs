// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

// Re-export PowerState for client convenience.
pub use drv_gimlet_state::PowerState;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum SeqError {
    IllegalTransition = 1,
    MuxToHostCPUFailed = 2,
    MuxToSPFailed = 3,
    ClockConfigFailed = 4,
    I2cError = 5,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
