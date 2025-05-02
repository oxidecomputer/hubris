// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;

pub use vsc85xx::{
    tesla::{TeslaSerdes6gObConfig, TeslaSerdes6gPatch},
    vsc8562::{Sd6gObCfg, Sd6gObCfg1},
};

pub use vsc7448::{
    config::{PortConfig, PortDev, PortMode, PortSerdes, Speed},
    VscError,
};

/// Maximum number of ports
pub const PORT_COUNT: usize = vsc7448::PORT_COUNT;

#[derive(Copy, Clone, Debug, Serialize, SerializedSize, Deserialize)]
#[repr(C)]
pub struct PortStatus {
    pub cfg: PortConfig,
    pub link_up: LinkStatus,
}

#[derive(Copy, Clone, Debug, Serialize, SerializedSize, Deserialize)]
#[repr(C)]
pub struct PacketCount {
    pub multicast: u32,
    pub unicast: u32,
    pub broadcast: u32,
}

#[derive(Copy, Clone, Debug, Serialize, SerializedSize, Deserialize)]
#[repr(C)]
pub struct PortCounters {
    pub rx: PacketCount,
    pub tx: PacketCount,

    /// `true` if the link has gone down since the last call to
    /// `port_reset_counters`
    ///
    /// Due to hardware differences between 1G and 10G ports, this has slightly
    /// different semantics depending on port speed:
    /// - On the 1G ports, this is `LINK_DOWN_STICKY` | `OUT_OF_SYNC_STICKY`
    /// - For the 10G port, this is `LOCK_CHANGED_STICKY`, i.e. it will _also_
    ///   be true if the link went from down -> up
    pub link_down_sticky: bool,

    /// `true` if this port has a PHY attached and the PHY's link down bit is
    /// set.  This is typically bit 13 in the interrupt status (0x1A) register.
    ///
    /// Note that the link down bit must be enabled in the interrupt mask
    /// register!
    pub phy_link_down_sticky: bool,
}

/// Error-code-only version of [VscError], for use in RPC calls
#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    FromPrimitive,
    ToPrimitive,
    IdolError,
    counters::Count,
)]
#[repr(C)]
pub enum MonorailError {
    SpiError = 1,
    ProxyError,
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
    PhyCommandError,
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
    OutOfRange,

    // ----------- Custom errors that aren't pulled from VscError -------------
    /// The given port is outside the valid port range
    InvalidPort,
    /// The given port is not configured
    UnconfiguredPort,
    /// The given port does not have a PHY associated with it
    NoPhy,

    /// The given operation is not supported
    NotSupported,

    #[idol(server_death)]
    ServerDied,
}

impl From<VscError> for MonorailError {
    fn from(e: VscError) -> Self {
        match e {
            VscError::SpiError(..) => Self::SpiError,
            VscError::ServerDied => Self::ServerDied,
            VscError::ProxyError(..) => Self::ProxyError,

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
            VscError::PhyCommandError(..) => Self::PhyCommandError,
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
            VscError::OutOfRange => Self::OutOfRange,
        }
    }
}

#[derive(
    Copy, Clone, Debug, Serialize, SerializedSize, Deserialize, Eq, PartialEq,
)]
#[repr(C)]
pub enum PhyType {
    Vsc8504,
    Vsc8522,
    Vsc8552,
    Vsc8562,
}

impl PhyType {
    /// Returns a mask of bits which must be set in register 20E3 for QSGMII
    /// to be considered okay
    pub fn qsgmii_okay_mask(&self) -> u16 {
        match self {
            // QSGMII sync, MAC comma detect
            PhyType::Vsc8504 | PhyType::Vsc8552 => 0b11 << 13,
            // SerDes signal detect
            PhyType::Vsc8522 => 1 << 14,
            // QSGMII sync, MAC comma detect, SerDes signal detect
            PhyType::Vsc8562 => 0b111 << 12,
        }
    }
}

#[derive(
    Copy, Clone, Debug, Serialize, SerializedSize, Deserialize, Eq, PartialEq,
)]
#[repr(C)]
pub enum LinkStatus {
    /// MAC_SYNC_FAIL or MAC_CGBAD is set
    Error,
    Down,
    Up,
}

#[derive(Copy, Clone, Debug, Serialize, SerializedSize, Deserialize)]
#[repr(C)]
pub struct PhyStatus {
    pub ty: PhyType,
    pub mac_link_up: LinkStatus,
    pub media_link_up: LinkStatus,
}

#[derive(
    Copy,
    Clone,
    Debug,
    zerocopy::IntoBytes,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
)]
#[repr(C)]
pub struct MacTableEntry {
    pub mac: [u8; 6],
    pub port: u16,
}

use crate as drv_monorail_api;
include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
