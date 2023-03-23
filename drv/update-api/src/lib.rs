// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use drv_caboose::CabooseError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};
use zerocopy::AsBytes;

// Re-export
pub use stage0_handoff::{
    HandoffDataLoadError, ImageVersion, RotBootState, RotImageDetails, RotSlot,
};

#[repr(u8)]
#[derive(
    FromPrimitive,
    AsBytes,
    Eq,
    PartialEq,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum UpdateTarget {
    // Represents targets where we only ever write to a single
    // alternate flash location. This is typically used in
    // conjunction with a bank swap feature.
    Alternate = 1,
    // Represents targets where we must write to a specific range
    // of flash.
    ImageA = 2,
    ImageB = 3,
    Bootloader = 4,
    // For testing
    DevNull = 0xff,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum UpdateStatus {
    LoadError(HandoffDataLoadError),
    Rot(RotBootState),
    // TODO(AJS): Fill in details for the SP
    Sp,
}

#[derive(Clone, Copy, FromPrimitive, IdolError, Serialize, Deserialize)]
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
    BadImageType = 14,
    UpdateAlreadyFinished = 15,
    UpdateNotStarted = 16,
    RunningImage = 17,
    FlashError = 18,
    MissingHeaderBlock = 19,
    InvalidHeaderBlock = 20,
    // Specific to RoT (LPC55)
    SpRotError = 21,

    #[idol(server_death)]
    ServerRestarted = 22,

    Unknown = 0xff,
}

impl hubpack::SerializedSize for UpdateError {
    const MAX_SIZE: usize = core::mem::size_of::<UpdateError>();
}

pub mod stm32h7 {
    // RM0433 Rev 7 section 4.3.9
    // Flash word is defined as 256 bits
    pub const FLASH_WORD_BITS: usize = 256;

    // Total length of a word in bytes (i.e. our array size)
    pub const FLASH_WORD_BYTES: usize = FLASH_WORD_BITS / 8;

    // This is arbitrarily chosen to determine how much data the server will
    // process at a time, and is not dictated by the hardware.
    pub const FLASH_WORDS_PER_BLOCK: usize = 32;

    // Block is an abstract concept here. It represents the size of data the
    // driver will process at a time.
    pub const BLOCK_SIZE_BYTES: usize =
        FLASH_WORD_BYTES * FLASH_WORDS_PER_BLOCK;

    pub const BLOCK_SIZE_WORDS: usize = BLOCK_SIZE_BYTES / 4;
}

pub mod lpc55 {
    // This value is currently set to `lpc55_romapi::FLASH_PAGE_SIZE`
    //
    // We hardcode it for simplicity, and because we cannot,and should not,
    // directly include the `lpc55_romapi` crate. While we could transfer
    // arbitrary amounts of data over spi and have the update server on
    // the RoT split it up, this makes the code more complicated than
    // necessary and is only an optimization. For now, especially since we
    // only have 1 RoT and we must currently define a constant for use the
    // `control_plane_agent::ComponentUpdater` trait, we do the simple thing
    // and hardcode according to hardware requirements.
    pub const BLOCK_SIZE_BYTES: usize = 512;
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
