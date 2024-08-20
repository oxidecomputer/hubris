// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Sequencer server.

#![no_std]

use counters::Count;
use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};

// Re-export PowerState for client convenience.
pub use drv_gimlet_state::PowerState;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, Count,
)]
pub enum SeqError {
    IllegalTransition = 1,
    MuxToHostCPUFailed,
    MuxToSPFailed,
    CPUNotPresent,
    UnrecognizedCPU,
    A1Timeout,
    A0TimeoutGroupC,
    A0Timeout,
    I2cFault,

    #[idol(server_death)]
    ServerRestarted,
}

// On Gimlet, we have two banks of up to 8 DIMMs apiece. Export the "two banks"
// bit of knowledge here so it can be used by gimlet-seq-server, spd, and
// packrat, all of which want to know at compile-time how many banks there are.
pub const NUM_SPD_BANKS: usize = 2;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
