// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Network Stack

#![no_std]

// For Idol-generated code that fully qualifies error type names.
use crate as task_net_api;
use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub use task_packrat_api::MacAddressBlock;

/// Errors that can occur when trying to send a packet.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum SendError {
    /// The outgoing tx queue is full. Wait until you get a notification that
    /// there is queue space and try again.
    QueueFull = 1,

    /// The server has restarted. Clients may or may not actually care about
    /// this; often you'll just want to retry, but because a netstack restart
    /// may imply one or more lost packets, we don't want to assume that.
    #[idol(server_death)]
    ServerRestarted = 2,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum RecvError {
    /// The incoming RX queue is empty; there are no packets able to be received
    /// from this socket. You can wait on the notification and try again if you
    /// like.
    QueueEmpty = 1,

    /// The server has restarted. Clients may or may not actually care about
    /// this; often you'll just want to retry, but because a netstack restart
    /// may imply one or more lost packets, we don't want to assume that.
    #[idol(server_death)]
    ServerRestarted = 2,
}

/// Errors that can occur when trying to set a VLAN as trusted
#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum TrustError {
    /// There is no such VLAN
    NoSuchVLAN = 1,

    /// The given VLAN is always trusted, and as such cannot be marked as
    /// temporarily trusted
    AlwaysTrusted,

    /// The server has restarted. Clients may or may not actually care about
    /// this; often you'll just want to retry, but because a netstack restart
    /// may imply one or more lost packets, we don't want to assume that.
    #[idol(server_death)]
    ServerRestarted,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum PhyError {
    /// The selected port is not valid
    InvalidPort = 1,

    /// This functionality isn't implemented
    NotImplemented = 2,

    Other = 3,

    #[idol(server_death)]
    ServerRestarted = 4,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum KszError {
    /// This functionality is not available on the given board
    NotAvailable = 1,
    /// The MAC table index is too large
    BadMacIndex,
    /// The given address is not a valid register
    BadRegister,

    WrongChipId,

    #[idol(server_death)]
    ServerRestarted,
}

#[derive(Copy, Clone, Debug, IntoBytes, Immutable, KnownLayout, FromBytes)]
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

#[derive(Copy, Clone, Debug, IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct MacAddress(pub [u8; 6]);

#[derive(
    Copy, Clone, Debug, Default, Serialize, SerializedSize, Deserialize,
)]
#[repr(C)]
pub struct ManagementLinkStatus {
    pub ksz8463_100base_fx_link_up: [bool; 2],
    pub vsc85x2_100base_fx_link_up: [bool; 2],
    pub vsc85x2_sgmii_link_up: [bool; 2],
}

#[derive(
    Copy, Clone, Debug, Default, Serialize, SerializedSize, Deserialize,
)]
#[repr(C)]
pub struct ManagementCountersVsc85x2 {
    pub mac_good: u16,
    pub media_good: u16,
    pub mac_bad: u16,
    pub media_bad: u16,
}

#[derive(
    Copy, Clone, Debug, Default, Serialize, SerializedSize, Deserialize,
)]
#[repr(C)]
pub struct ManagementCountersKsz8463 {
    pub multicast: u32,
    pub unicast: u32,
    pub broadcast: u32,
}

#[derive(
    Copy, Clone, Debug, Default, Serialize, SerializedSize, Deserialize,
)]
#[repr(C)]
pub struct ManagementCounters {
    pub vsc85x2_tx: [ManagementCountersVsc85x2; 2],
    pub vsc85x2_rx: [ManagementCountersVsc85x2; 2],

    pub ksz8463_tx: [ManagementCountersKsz8463; 3],
    pub ksz8463_rx: [ManagementCountersKsz8463; 3],

    /// The MAC counters are only valid on the VSC8562
    pub vsc85x2_mac_valid: bool,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IdolError, counters::Count,
)]
#[repr(u32)]
pub enum MgmtError {
    NotAvailable = 1,
    VscError,
    KszError,

    #[idol(server_death)]
    ServerRestarted,
}

////////////////////////////////////////////////////////////////////////////////

#[derive(
    Copy, Clone, Debug, Serialize, SerializedSize, Deserialize, PartialEq, Eq,
)]
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

#[derive(
    Copy, Clone, Serialize, SerializedSize, Deserialize, PartialEq, Eq,
)]
pub struct UdpMetadata {
    pub addr: Address,
    pub port: u16,
    pub size: u32,

    #[cfg(feature = "vlan")]
    pub vid: VLanId,
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
#[derive(
    Copy, Clone, Debug, Serialize, SerializedSize, Deserialize, PartialEq, Eq,
)]
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
        }
    }
}

#[cfg(feature = "use-smoltcp")]
pub struct AddressUnspecified;

#[derive(
    Copy, Clone, Debug, Serialize, SerializedSize, Deserialize, PartialEq, Eq,
)]
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

/// Upstream SP port
///
/// Values are based on the KSZ8463's numbering (1-3); port 3 is connected to
/// the SP itself and cannot be used as a source / destination.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum SpPort {
    One,
    Two,
}

/// Configuration for a SP VLAN
#[derive(Copy, Clone)]
pub struct VLanConfig {
    /// VLAN VID
    pub vid: u16,

    /// Whether this VLAN is always trusted
    pub always_trusted: bool,

    /// SP port associated with this VLAN
    ///
    /// In rare cases, multiple VLANs can be associated with the same SP port
    pub port: SpPort,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/net_config.rs"));
