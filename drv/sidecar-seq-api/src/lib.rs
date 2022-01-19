// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Sidecar Sequencer server.

#![no_std]

use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
pub enum SeqError {
    IllegalTransition = 1,
    ClockConfigFailed = 2,
}

impl From<SeqError> for u16 {
    fn from(rc: SeqError) -> Self {
        rc as u16
    }
}

impl core::convert::TryFrom<u32> for SeqError {
    type Error = ();
    fn try_from(rc: u32) -> Result<Self, Self::Error> {
        Self::from_u32(rc).ok_or(())
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum PowerState {
    A2 = 1,
    A0 = 2,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
