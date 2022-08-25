// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};

#[derive(FromPrimitive, IdolError)]
#[repr(u32)]
pub enum UpdateError {
    BadLength = 1,
    UpdateInProgress = 2,
    OutOfBounds = 3,
    Timeout = 4,
    // Specific to STM32H7
    EccDoubleErr = 5,
    EccSingleErr = 6,
    SecureErr = 7,   // If we get this something has gone very wrong
    ReadProtErr = 8, // If we get this something has gone very wrong
    WriteEraseErr = 9,
    InconsistencyErr = 10,
    StrobeErr = 11,
    ProgSeqErr = 12,
    WriteProtErr = 13,
}

pub mod stm32h7 {
    // RM0433 Rev 7 section 4.3.9
    // Flash word is defined as 256 bits
    pub const FLASH_WORD_BITS: usize = 256;

    // Total length of a word in bytes (i.e. our array size)
    pub const FLASH_WORD_BYTES: usize = FLASH_WORD_BITS / 8;

    // Block is an abstract concept here. It represents the size of data the
    // driver will process at a time.
    pub const BLOCK_SIZE_BYTES: usize = FLASH_WORD_BYTES * 32;
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
