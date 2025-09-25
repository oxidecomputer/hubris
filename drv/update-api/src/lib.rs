// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use gateway_messages::UpdateError as GwUpdateError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::FromPrimitive;

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
#[derive(counters::Count)]
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

    MissingHandoffData,
    BlockOutOfOrder,
    InvalidSlotIdForOperation,
    ImageMismatch,
    SignatureNotValidated,
    VersionNotSupported,

    InvalidPreferredSlotId,
    AlreadyPending,
    NonePending,
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
            UpdateError::MissingHandoffData => Self::MissingHandoffData,
            UpdateError::BlockOutOfOrder => Self::BlockOutOfOrder,
            UpdateError::InvalidSlotIdForOperation => {
                Self::InvalidSlotIdForOperation
            }
            UpdateError::ImageMismatch => Self::ImageMismatch,
            UpdateError::SignatureNotValidated => Self::SignatureNotValidated,
            UpdateError::VersionNotSupported => Self::VersionNotSupported,
            UpdateError::InvalidPreferredSlotId => Self::InvalidPreferredSlotId,
            UpdateError::AlreadyPending => Self::AlreadyPending,
            UpdateError::NonePending => Self::NonePending,
        }
    }
}
