// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the host / SP communications task.

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

pub use host_sp_messages::{HostStartupOptions, Status};

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum HostSpCommsError {
    InvalidStatus = 1,
    InvalidStartupOptions,

    #[idol(server_death)]
    ServerRestarted,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
