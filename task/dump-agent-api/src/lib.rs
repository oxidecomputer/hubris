// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dump Agent task.

#![no_std]

use userlib::*;
use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum DumpAgentError {
    InvalidArea = 1,
    BadOffset = 2,
}

#[derive(Copy, Clone, Debug, SerializedSize, Serialize, Deserialize)]
pub struct DumpArea {
    pub address: u32,
    pub length: u32,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
