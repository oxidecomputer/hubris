// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::{FromPrimitive, ToPrimitive};

pub use vsc7448::{
    config::{PortConfig, PortDev, PortMode},
    VscError,
};

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct PortStatus {
    pub cfg: PortConfig,
    pub link_up: bool,
}

/// Error-code-only version of [VscError], for use in RPC calls
#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, IdolError,
)]
pub enum MonorailError {
    SpiError,
    ServerDied,
    BadChipId,
    Serdes1gReadTimeout,
    Serdes1gWriteTimeout,
    Serdes6gReadTimeout,
    Serdes6gWriteTimeout,
    PortFlushTimeout,
    AnaCfgTimeout,
    SerdesFrequencyTooLow,
    SerdesFrequencyTooHigh,
    TriDecFailed,
    BiDecFailed,
    LtDecFailed,
    LsDecFailed,
    TxPllLockFailed,
    TxPllFsmFailed,
    RxPllLockFailed,
    RxPllFsmFailed,
    OffsetCalFailed,
    InvalidDev1g,
    InvalidDev2g5,
    InvalidDev10g,
    LcPllInitFailed,
    CalConfigFailed,
    RamInitFailed,
    TooMuchBandwidth,
    BadPhyId,
    BadPhyRev,
    BadPhyPatchPort,
    PhyPatchFailedCrc,
    PhyInitTimeout,
    UnknownPhyId,
    MacSecWaitTimeout,
    McbReadTimeout,
    McbWriteTimeout,
    PhyPllCalTimeout,
    PhyIbCalTimeout,
    BadRegAddr,
    InvalidRegisterRead,
    InvalidRegisterReadNested,
    MiimReadErr,
    MiimIdleTimeout,
    MiimReadTimeout,

    // Custom errors that aren't pulled from VscError
    InvalidPort,
    UnconfiguredPort,
}

impl From<VscError> for MonorailError {
    fn from(e: VscError) -> Self {
        match e {
            VscError::SpiError(..) => Self::SpiError,
            VscError::ServerDied => Self::ServerDied,

            VscError::BadChipId(..) => Self::BadChipId,
            VscError::Serdes1gReadTimeout { .. } => Self::Serdes1gReadTimeout,
            VscError::Serdes1gWriteTimeout { .. } => Self::Serdes1gWriteTimeout,
            VscError::Serdes6gReadTimeout { .. } => Self::Serdes6gReadTimeout,
            VscError::Serdes6gWriteTimeout { .. } => Self::Serdes6gWriteTimeout,
            VscError::PortFlushTimeout { .. } => Self::PortFlushTimeout,
            VscError::AnaCfgTimeout => Self::AnaCfgTimeout,
            VscError::SerdesFrequencyTooLow(..) => Self::SerdesFrequencyTooLow,
            VscError::SerdesFrequencyTooHigh(..) => {
                Self::SerdesFrequencyTooHigh
            }
            VscError::TriDecFailed(..) => Self::TriDecFailed,
            VscError::BiDecFailed(..) => Self::BiDecFailed,
            VscError::LtDecFailed(..) => Self::LtDecFailed,
            VscError::LsDecFailed(..) => Self::LsDecFailed,
            VscError::TxPllLockFailed => Self::TxPllLockFailed,
            VscError::TxPllFsmFailed => Self::TxPllFsmFailed,
            VscError::RxPllLockFailed => Self::RxPllLockFailed,
            VscError::RxPllFsmFailed => Self::RxPllFsmFailed,
            VscError::OffsetCalFailed => Self::OffsetCalFailed,
            VscError::InvalidDev1g(..) => Self::InvalidDev1g,
            VscError::InvalidDev2g5(..) => Self::InvalidDev2g5,
            VscError::InvalidDev10g(..) => Self::InvalidDev10g,
            VscError::LcPllInitFailed(..) => Self::LcPllInitFailed,
            VscError::CalConfigFailed => Self::CalConfigFailed,
            VscError::RamInitFailed => Self::RamInitFailed,
            VscError::TooMuchBandwidth(..) => Self::TooMuchBandwidth,
            VscError::BadPhyId(..) => Self::BadPhyId,
            VscError::BadPhyRev => Self::BadPhyRev,
            VscError::BadPhyPatchPort(..) => Self::BadPhyPatchPort,
            VscError::PhyPatchFailedCrc => Self::PhyPatchFailedCrc,
            VscError::PhyInitTimeout => Self::PhyInitTimeout,
            VscError::UnknownPhyId(..) => Self::UnknownPhyId,

            VscError::MacSecWaitTimeout => Self::MacSecWaitTimeout,
            VscError::McbReadTimeout => Self::McbReadTimeout,
            VscError::McbWriteTimeout => Self::McbWriteTimeout,
            VscError::PhyPllCalTimeout => Self::PhyPllCalTimeout,
            VscError::PhyIbCalTimeout => Self::PhyIbCalTimeout,

            VscError::BadRegAddr(..) => Self::BadRegAddr,
            VscError::InvalidRegisterRead(..) => Self::InvalidRegisterRead,
            VscError::InvalidRegisterReadNested => {
                Self::InvalidRegisterReadNested
            }

            VscError::MiimReadErr { .. } => Self::MiimReadErr,
            VscError::MiimIdleTimeout => Self::MiimIdleTimeout,
            VscError::MiimReadTimeout => Self::MiimReadTimeout,
        }
    }
}
