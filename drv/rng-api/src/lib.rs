// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the random number generator.

#![no_std]

use derive_idol_err::IdolError;
use userlib::{FromPrimitive, sys_send};

#[repr(u32)]
#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum RngError {
    PoweredOff = 1,
    NoData,
    ClockError,
    SeedError,
    UnknownRngError,

    #[idol(server_death)]
    ServerRestarted,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
