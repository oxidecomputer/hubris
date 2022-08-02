// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Network Stack

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::*;

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
