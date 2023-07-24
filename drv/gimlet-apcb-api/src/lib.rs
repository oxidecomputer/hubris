// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet APCB server.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::AsBytes;

pub use drv_qspi_api::{PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

/// Errors that can be produced from the APCB server API.
///
/// This enumeration doesn't include errors that result from configuration
/// issues, like sending APCB messages to some other task.
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum ApcbError {
    FIXME = 1,

    #[idol(server_death)]
    ServerRestarted,
}

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    AsBytes,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(u16)]
pub enum ApcbWellKnownEffect {
    BmcEnable,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
