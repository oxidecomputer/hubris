// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Network Stack

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::{AsBytes, FromBytes, LittleEndian, U16};

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError)]
#[repr(u32)]
pub enum SendError {
    /// The selected socket is not owned by this task
    NotYours = 1,

    /// The specified VID is not in the configured range
    InvalidVLan = 2,

    /// The outgoing tx queue is full
    QueueFull = 3,

    Other = 4,

    #[idol(server_death)]
    ServerRestarted = 5,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError)]
#[repr(u32)]
pub enum RecvError {
    /// The selected socket is not owned by this task
    NotYours = 1,

    /// The incoming rx queue is empty
    QueueEmpty = 2,

    Other = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError)]
#[repr(u32)]
pub enum PhyError {
    /// The selected port is not valid
    InvalidPort = 1,

    /// This functionality isn't implemented
    NotImplemented = 2,

    Other = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError)]
#[repr(u32)]
pub enum KszError {
    /// This functionality is not available on the given board
    NotAvailable = 1,
    /// The MAC table index is too large
    BadMacIndex,
    /// The given address is not a valid register
    BadRegister,

    WrongChipId,

    // Errors copied from SpiError
    BadTransferSize,
    ServerRestarted,
    NothingToRelease,
    BadDevice,
    DataOverrun,
}

#[cfg(feature = "ksz8463")]
impl From<ksz8463::Error> for KszError {
    fn from(e: ksz8463::Error) -> Self {
        use drv_spi_api::SpiError;
        match e {
            ksz8463::Error::SpiError(e) => match e {
                SpiError::BadTransferSize => KszError::BadTransferSize,
                SpiError::ServerRestarted => KszError::ServerRestarted,
                SpiError::NothingToRelease => KszError::NothingToRelease,
                SpiError::BadDevice => KszError::BadDevice,
                SpiError::DataOverrun => KszError::DataOverrun,
            },
            ksz8463::Error::WrongChipId(..) => KszError::WrongChipId,
        }
    }
}

#[derive(Copy, Clone, Debug, AsBytes, FromBytes)]
#[repr(C)]
pub struct KszMacTableEntry {
    pub mac: [u8; 6],
    pub port: u16,
}

#[cfg(feature = "ksz8463")]
impl From<ksz8463::KszRawMacTableEntry> for KszMacTableEntry {
    fn from(e: ksz8463::KszRawMacTableEntry) -> Self {
        Self {
            mac: e.addr,
            port: match e.source {
                ksz8463::SourcePort::Port1 => 1,
                ksz8463::SourcePort::Port2 => 2,
                ksz8463::SourcePort::Port3 => 3,
            },
        }
    }
}

#[derive(Copy, Clone, Debug, AsBytes, FromBytes)]
#[repr(C)]
pub struct MacAddress(pub [u8; 6]);

/// Represents a range of allocated MAC addresses, per RFD 320
///
/// The SP will claim the first `N` addresses based on VLAN configuration
/// (typically either 1 or 2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromBytes, AsBytes, Default)]
#[repr(C)]
pub struct MacAddressBlock {
    pub base_mac: [u8; 6],
    pub count: U16<LittleEndian>,
    pub stride: u8,
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct ManagementLinkStatus {
    pub ksz8463_100base_fx_link_up: [bool; 2],
    pub vsc85x2_100base_fx_link_up: [bool; 2],
    pub vsc85x2_sgmii_link_up: [bool; 2],
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct ManagementCountersVsc85x2 {
    pub mac_good: u16,
    pub media_good: u16,
    pub mac_bad: u16,
    pub media_bad: u16,
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct ManagementCountersKsz8463 {
    pub multicast: u32,
    pub unicast: u32,
    pub broadcast: u32,
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct ManagementCounters {
    pub vsc85x2_tx: [ManagementCountersVsc85x2; 2],
    pub vsc85x2_rx: [ManagementCountersVsc85x2; 2],

    pub ksz8463_tx: [ManagementCountersKsz8463; 3],
    pub ksz8463_rx: [ManagementCountersKsz8463; 3],

    /// The MAC counters are only valid on the VSC8562
    pub vsc85x2_mac_valid: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError)]
#[repr(u32)]
pub enum MgmtError {
    NotAvailable = 1,
    VscError,
    KszError,
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u32)]
pub enum LargePayloadBehavior {
    /// If we have a packet with a payload larger than the buffer provided to
    /// `recv()`, discard it.
    Discard,
    // We could add a `Fail` case here allowing callers to retry with a
    // larger payload buffer, but
    //
    // a) we have no callers that want to do this today, and
    // b) it complicates the net implementation
    //
    // so we omit it for now.
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UdpMetadata {
    pub addr: Address,
    pub port: u16,
    pub size: u32,

    #[cfg(feature = "vlan")]
    pub vid: u16,
}

#[cfg(feature = "use-smoltcp")]
impl From<UdpMetadata> for smoltcp::wire::IpEndpoint {
    fn from(m: UdpMetadata) -> Self {
        Self {
            addr: m.addr.into(),
            port: m.port,
        }
    }
}

// This must be repr(C); otherwise Rust cleverly optimizes out the enum tag,
// which breaks ssmarshal's assumptions about struct sizes.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[repr(C)]
pub enum Address {
    Ipv6(Ipv6Address),
}

#[cfg(feature = "use-smoltcp")]
impl From<Address> for smoltcp::wire::IpAddress {
    fn from(a: Address) -> Self {
        match a {
            Address::Ipv6(a) => Self::Ipv6(a.into()),
        }
    }
}

#[cfg(feature = "use-smoltcp")]
impl TryFrom<smoltcp::wire::IpAddress> for Address {
    type Error = AddressUnspecified;

    fn try_from(a: smoltcp::wire::IpAddress) -> Result<Self, Self::Error> {
        use smoltcp::wire::IpAddress;

        match a {
            IpAddress::Ipv6(a) => Ok(Self::Ipv6(a.into())),
            _ => Err(AddressUnspecified),
        }
    }
}

#[cfg(feature = "use-smoltcp")]
pub struct AddressUnspecified;

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Ipv6Address(pub [u8; 16]);

#[cfg(feature = "use-smoltcp")]
impl From<smoltcp::wire::Ipv6Address> for Ipv6Address {
    fn from(a: smoltcp::wire::Ipv6Address) -> Self {
        Self(a.0)
    }
}

#[cfg(feature = "use-smoltcp")]
impl From<Ipv6Address> for smoltcp::wire::Ipv6Address {
    fn from(a: Ipv6Address) -> Self {
        Self(a.0)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/net_config.rs"));
