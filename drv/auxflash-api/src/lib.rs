// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum AuxFlashError {
    WriteEnableFailed = 1,
    ServerRestarted = 2,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
