// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum SeqError {
    IllegalTransition = 1,
    MuxToHostCPUFailed = 2,
    MuxToSPFailed = 3,
    ClockConfigFailed = 4,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum PowerState {
    A2 = 1,
    A2PlusMono = 2,
    A2PlusFans = 3,
    A1 = 4,
    A0 = 5,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
