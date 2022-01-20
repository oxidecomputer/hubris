// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Network Stack

#![no_std]

use serde::{Deserialize, Serialize};
use userlib::*;

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum NetError {
    QueueEmpty = 1,
    NotYours = 2,
}

impl From<u32> for NetError {
    fn from(x: u32) -> Self {
        match x {
            1 => NetError::QueueEmpty,
            _ => panic!(),
        }
    }
}

impl From<NetError> for u16 {
    fn from(x: NetError) -> Self {
        x as u16
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct UdpMetadata {
    pub addr: Address,
    pub port: u16,
    pub size: u32,
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

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
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
