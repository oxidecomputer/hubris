// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};

#[derive(
    Debug, Clone, Copy, FromPrimitive, Deserialize, Serialize, SerializedSize,
)]
pub enum Device {
    Mwocp68,
}

#[derive(
    Debug, Clone, Copy, FromPrimitive, Deserialize, Serialize, SerializedSize,
)]
pub enum Operation {
    Todo1,
    Todo2,
}

#[derive(
    Debug, Clone, Copy, FromPrimitive, Deserialize, Serialize, SerializedSize,
)]
pub enum Value {
    Todo1,
    Todo2,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum PowerError {
    Todo = 1,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
