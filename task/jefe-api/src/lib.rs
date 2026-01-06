// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for El Jefe

#![no_std]

use derive_idol_err::IdolError;
pub use dump_agent_api::DumpAgentError;
use serde::{Deserialize, Serialize};
use userlib::*;

/// Platform-agnostic (but heavily influenced) reset status bits.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, counters::Count,
)]
#[repr(C)]
pub enum ResetReason {
    PowerOn,
    Pin,
    SystemCall,
    Brownout,
    SystemWatchdog,
    IndependentWatchdog,
    LowPowerSecurity,
    ExitStandby,
    Other(u32),
    Unknown, // TODO remove and use `Option<ResetReason>` once we switch to hubpack
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
#[repr(C)]
pub enum DumpAreaError {
    InvalidIndex = 1,
    AlreadyInUse,
}

impl Jefe {
    /// Asks the supervisor to restart the current task without recording a
    /// fault.
    pub fn restart_me(&self) -> ! {
        self.restart_me_raw();
        unreachable!()
    }
}

pub type TaskFaultCounts = [usize; hubris_num_tasks::NUM_TASKS];

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
