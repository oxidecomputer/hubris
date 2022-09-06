// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the auxiliary flash IC

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;
use zerocopy::AsBytes;

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
    /// The `CHCK` checksum disagrees with the actual slot data (`AUXI`)
    ChckMismatch,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
