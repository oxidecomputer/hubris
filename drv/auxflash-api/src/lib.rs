// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the auxiliary flash IC

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum AuxFlashError {
    WriteEnableFailed = 1,
    ServerRestarted,
    TlvcReaderBeginFailed,

    /// The requested slot exceeds the slot count
    InvalidSlot,
    /// The `CHCK` block does not have 32 bytes of data
    BadChckSize,
    /// There is no `CHCK` block in this slot
    MissingChck,
    /// There is no `AUXI` block in this slot
    MissingAuxi,
    /// There is more than one `CHCK` block in this slot
    MultipleChck,
    /// There is more than one `AUXI` block in this slot
    MultipleAuxi,
    /// The `CHCK` checksum disagrees with the actual slot data (`AUXI`)
    ChckMismatch,
    /// Failed during a call to `ChunkHandle::read_exact`
    ChunkReadFail,
    /// The end address of the read or write exceeds the slot boundaries
    AddressOverflow,
    /// The start address of a write command is not aligned to a page boundary
    UnalignedAddress,
    /// There is no active slot
    NoActiveSlot,
}

#[derive(Copy, Clone, zerocopy::FromBytes, zerocopy::AsBytes)]
#[repr(transparent)]
pub struct AuxFlashId(pub [u8; 20]);

#[derive(Copy, Clone, zerocopy::FromBytes, zerocopy::AsBytes)]
#[repr(transparent)]
pub struct AuxFlashChecksum(pub [u8; 32]);

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
