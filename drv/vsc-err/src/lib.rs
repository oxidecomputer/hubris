// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This crate provides an error type that is used by our PHYs and switches.
//! It is factored into its own crate so that it can be used by both
//! `drv/vsc7448` and `drv/vsc85xx` without introducing any unneeded
//! dependencies in each case.

#![no_std]

use drv_spi_api::SpiError;
use idol_runtime::ServerDeath;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum VscError {
    SpiError(SpiError),
    ServerDied,
    /// Error code produced by a proxy device handling PHY register
    /// reads/writes.
    ProxyError(u16),

    BadChipId(u32),
    Serdes1gReadTimeout {
        instance: u8,
    },
    Serdes1gWriteTimeout {
        instance: u8,
    },
    Serdes6gReadTimeout {
        instance: u8,
    },
    Serdes6gWriteTimeout {
        instance: u8,
    },
    PortFlushTimeout {
        port: u8,
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
    InvalidDev1g(u8),
    InvalidDev2g5(u8),
    InvalidDev10g(u8),
    LcPllInitFailed(u8),
    CalConfigFailed,
    RamInitFailed,
    TooMuchBandwidth(usize),

    /// Mismatch in the `IDENTIFIER_1/2` PHY register
    BadPhyId(u32),
    /// Indicates that the VSC8504 is not Tesla E silicon
    BadPhyRev,
    /// Indicates that we tried to apply the phy patch to an invalid port;
    /// it can only be applied to port 0 of the PHY
    BadPhyPatchPort(u16),
    /// Checking the CRC after applying a patch to the PHY firmware returned
    /// an unexpected CRC.
    PhyPatchFailedCrc,
    PhyInitTimeout,
    /// An error was returned when executing a Phy command
    PhyCommandError(u16),
    /// Returned by functions that support both the VSC8552 and VSC8562, when
    /// the PHY id doesn't match either.
    UnknownPhyId(u32),

    /// The MACSEC block failed to finish an operation in time
    MacSecWaitTimeout,
    /// The MCB module in the PHY timed out while doing a read
    McbReadTimeout,
    /// The MCB module in the PHY timed out while doing a write
    McbWriteTimeout,
    /// We timed out while doing a calibration step in a PHY PLL
    PhyPllCalTimeout,
    /// We timed out while doing input buffer calibration on a PHY
    PhyIbCalTimeout,

    BadRegAddr(u32),
    InvalidRegisterRead(u32),
    InvalidRegisterReadNested,

    MiimReadErr {
        miim: u8,
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

impl From<ServerDeath> for VscError {
    fn from(_s: ServerDeath) -> Self {
        Self::ServerDied
    }
}
