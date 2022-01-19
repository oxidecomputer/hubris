// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This crate provides an error type that is used by our PHYs and switches.
//! It is factored into its own crate so that it can be used by both
//! `drv/vsc7448` and `drv/vsc85xx` without introducing any unneeded
//! dependencies in each case.

#![no_std]

use drv_spi_api::SpiError;

#[derive(Copy, Clone, PartialEq)]
pub enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
    Serdes1gReadTimeout {
        instance: u32,
    },
    Serdes1gWriteTimeout {
        instance: u32,
    },
    Serdes6gReadTimeout {
        instance: u32,
    },
    Serdes6gWriteTimeout {
        instance: u32,
    },
    PortFlushTimeout {
        port: u32,
    },
    AnaCfgTimeout,
    SerdesFrequencyTooLow(u64),
    SerdesFrequencyTooHigh(u64),
    TriDecFailed(u16),
    BiDecFailed(u16),
    LtDecFailed(u16),
    LsDecFailed(u16),
    TxPllLockFailed,
    TxPllFsmFailed,
    RxPllLockFailed,
    RxPllFsmFailed,
    OffsetCalFailed,

    /// Mismatch in the `IDENTIFIER_1` PHY register
    BadId1(u16),
    /// Mismatch in the `IDENTIFIER_2` PHY register
    BadId2(u16),
    /// Indicates that the VSC8504 is not Tesla E silicon
    BadRev,
    InitTimeout,

    MiimReadErr {
        miim: u32,
        phy: u8,
        page: u16,
        addr: u8,
    },
    MiimIdleTimeout,
    MiimReadTimeout,
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}
