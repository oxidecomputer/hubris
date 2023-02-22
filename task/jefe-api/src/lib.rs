// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for El Jefe

#![no_std]

use serde::{Deserialize, Serialize};
use userlib::*;
use humpty::DumpAreaHeader;
use derive_idol_err::IdolError;

/// Platform-agnostic (but heavily influenced) reset status bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(C)]
pub enum DumpAreaHeaderError {
    InvalidIndex = 1,
    AlreadyInUse,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
