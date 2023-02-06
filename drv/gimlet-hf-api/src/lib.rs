// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use derive_idol_err::IdolError;
use drv_hash_api::SHA256_SZ;
use userlib::*;
use zerocopy::AsBytes;

pub use drv_qspi_api::{PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

/// Errors that can be produced from the host flash server API.
///
/// This enumeration doesn't include errors that result from configuration
/// issues, like sending host flash messages to some other task.
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum HfError {
    WriteEnableFailed = 1,
    HashBadRange,
    HashError,
    HashNotConfigured,
    NoDevSelect,
    NotMuxedToSP,
    Sector0IsReserved,

    #[idol(server_death)]
    ServerRestarted,
}

/// Controls whether the SP or host CPU has access to flash
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, AsBytes)]
#[repr(u8)]
pub enum HfMuxState {
    SP = 1,
    HostCPU = 2,
}

/// Selects between multiple flash chips. This is not used on all hardware
/// revisions; it was added in Gimlet rev B.
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, AsBytes)]
#[repr(u8)]
pub enum HfDevSelect {
    Flash0 = 0,
    Flash1 = 1,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
