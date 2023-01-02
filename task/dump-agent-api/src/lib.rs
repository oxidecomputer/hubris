// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dump Agent task.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;

pub use humpty::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum DumpAgentError {
    InvalidArea = 1,
    BadOffset = 2,
    UnalignedOffset = 3,
    UnalignedSegmentAddress = 4,
    UnalignedSegmentLength = 5,
}

#[derive(Copy, Clone, Debug, SerializedSize, Serialize, Deserialize)]
pub struct DumpArea {
    pub address: u32,
    pub length: u32,
}

pub const DUMP_READ_SIZE: usize = 256;
pub const DUMP_AGENT_VERSION: u8 = 1_u8;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
