// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use derive_idol_err::IdolError;
use drv_hash_api::SHA256_SZ;
use userlib::*;
use zerocopy::AsBytes;

/// Errors that can be produced from the host flash server API.
///
/// This enumeration doesn't include errors that result from configuration
/// issues, like sending host flash messages to some other task.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum HfError {
    WriteEnableFailed = 1,
    ServerRestarted = 2,
    MuxFailed = 3,
    HashBadRange = 4,
    HashError = 5,
    HashNotConfigured = 6,
    NoDevSelect = 7,
    DevSelectFailed = 8,
    NotMuxedToSP = 9,
}

/// Controls whether the SP or host CPU has access to flash
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum HfMuxState {
    SP = 1,
    HostCPU = 2,
}

/// Selects between multiple flash chips. This is not used on all hardware
/// revisions; it was added in Gimlet rev B.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum HfDevSelect {
    Flash0 = 0,
    Flash1 = 1,
}

/// Size in bytes of a single page of data (i.e., the max length of slice we
/// accept for `page_program()` and `read()`).
// Note: There is no static check that this matches what's in our idl file in
// terms of _client_ generation, but the server can use this constant in its
// trait impl, which will produce a compile-time error if it doesn't match the
// length in the idl file.
pub const PAGE_SIZE_BYTES: usize = 256;

/// Size in bytes of a single sector of data (i.e., the size of the data erased
/// by a call to `sector_erase()`).
pub const SECTOR_SIZE_BYTES: usize = 65_536;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
