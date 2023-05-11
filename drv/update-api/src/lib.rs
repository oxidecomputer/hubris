// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use drv_caboose::CabooseError;
use gateway_messages::UpdateError as GwUpdateError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};
use zerocopy::AsBytes;

// Re-export
pub use stage0_handoff::{
    HandoffDataLoadError, ImageVersion, RotBootState, RotImageDetails, RotSlot,
};

/// Target for an update operation
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
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

// These values are used as raw integers in the `State::Failed(UpdateError)`
// variant.  To preserve compatibility, DO NOT REORDER THEM.
// N.B These varients must be kept in order to maintain compatibility between
// skewed versions of SP and RoT during updates.
#[derive(
    Debug,
    Clone,
    Copy,
    FromPrimitive,
    IdolError,
    Serialize,
    Deserialize,
    PartialEq,
    SerializedSize,
)]
#[repr(u32)]
pub enum UpdateError {
    BadLength = 1,
    UpdateInProgress,
    OutOfBounds,
    EccDoubleErr,
    EccSingleErr,
    SecureErr,   // If we get this something has gone very wrong
    ReadProtErr, // If we get this something has gone very wrong
    WriteEraseErr,
    InconsistencyErr,
    StrobeErr,
    ProgSeqErr,
    WriteProtErr,
    BadImageType,
    UpdateAlreadyFinished,
    UpdateNotStarted,
    RunningImage,
    FlashError,
    FlashIllegalRead,
    FlashReadFail,
    MissingHeaderBlock,
    InvalidHeaderBlock,

    // Caboose checks
    ImageBoardMismatch,
    ImageBoardUnknown,

    #[idol(server_death)]
    TaskRestarted,

    NotImplemented,
}

impl From<UpdateError> for GwUpdateError {
    fn from(value: UpdateError) -> Self {
        match value {
            UpdateError::BadLength => Self::BadLength,
            UpdateError::UpdateInProgress => Self::UpdateInProgress,
            UpdateError::OutOfBounds => Self::OutOfBounds,
            UpdateError::EccDoubleErr => Self::EccDoubleErr,
            UpdateError::EccSingleErr => Self::EccSingleErr,
            UpdateError::SecureErr => Self::SecureErr,
            UpdateError::ReadProtErr => Self::ReadProtErr,
            UpdateError::WriteEraseErr => Self::WriteEraseErr,
            UpdateError::InconsistencyErr => Self::InconsistencyErr,
            UpdateError::StrobeErr => Self::StrobeErr,
            UpdateError::ProgSeqErr => Self::ProgSeqErr,
            UpdateError::WriteProtErr => Self::WriteProtErr,
            UpdateError::BadImageType => Self::BadImageType,
            UpdateError::UpdateAlreadyFinished => Self::UpdateAlreadyFinished,
            UpdateError::UpdateNotStarted => Self::UpdateNotStarted,
            UpdateError::RunningImage => Self::RunningImage,
            UpdateError::FlashError => Self::FlashError,
            UpdateError::FlashIllegalRead => Self::FlashIllegalRead,
            UpdateError::FlashReadFail => Self::FlashReadFail,
            UpdateError::MissingHeaderBlock => Self::MissingHeaderBlock,
            UpdateError::InvalidHeaderBlock => Self::InvalidHeaderBlock,
            UpdateError::ImageBoardMismatch => Self::ImageBoardMismatch,
            UpdateError::ImageBoardUnknown => Self::ImageBoardUnknown,
            UpdateError::TaskRestarted => Self::TaskRestarted,
            UpdateError::NotImplemented => Self::NotImplemented,
        }
    }
}

/// When booting into an alternate image, specifies how "sticky" that decision
/// is.
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum SwitchDuration {
    /// Choice applies once. Resetting the processor will return to the original
    /// image. Useful when provisionally testing an update, but only available
    /// on certain implementations.
    Once,
    /// Choice is permanent until changed. This is more dangerous, but is also
    /// universally available.
    Forever,
}

/// Designates a firmware image slot in parts that have fixed slots (rather than
/// bank remapping).
///
/// This `enum` is used as part of the wire format for SP-RoT communication, and
/// therefore cannot be changed at will; see discussion in `drv_sprot_api::Msg`
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    Serialize,
    Deserialize,
    SerializedSize,
)]
pub enum SlotId {
    A,
    B,
}

impl TryFrom<u16> for SlotId {
    type Error = ();
    fn try_from(i: u16) -> Result<Self, Self::Error> {
        match i {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
            _ => Err(()),
        }
    }
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

// Allow our Idol definition to fully specify API structures
use crate as drv_update_api;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
