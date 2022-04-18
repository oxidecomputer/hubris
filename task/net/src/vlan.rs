// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation with VLAN support
//!
//! This module implements a server which listens on a single IPv6 address
//! and supports some number of VLANs
//!
//! TODO: one IPv6 address per VLAN?

use drv_stm32h7_eth as eth;

use idol_runtime::{ClientError, NotificationHandler, RequestError};
use smoltcp::iface::{Interface, Neighbor, SocketHandle, SocketStorage};
use smoltcp::socket::UdpSocket;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv6Address};
use task_net_api::{NetError, SocketName, UdpMetadata};
use userlib::{sys_post, sys_refresh_task_id};

use crate::generated::{self, INSTANCE_COUNT, SOCKET_COUNT};
use crate::{ETH_IRQ, NEIGHBORS, WAKE_IRQ};

/// Storage required to run a single [ServerImpl]. This should be allocated
/// on the stack and passed into the constructor for the [ServerImpl].
pub struct ServerStorage<'a> {
    ipv6_addr: Ipv6Address,
    neighbor_cache_storage:
        [[Option<(IpAddress, Neighbor)>; NEIGHBORS]; INSTANCE_COUNT],
    socket_storage: [[SocketStorage<'a>; SOCKET_COUNT]; INSTANCE_COUNT],
    ipv6_net: [IpCidr; INSTANCE_COUNT],
}

impl<'a> ServerStorage<'a> {
    pub fn new(ipv6_addr: Ipv6Address) -> Self {
        let ipv6_net = smoltcp::wire::Ipv6Cidr::new(ipv6_addr, 64).into();
        Self {
            ipv6_addr,
            neighbor_cache_storage: [[None; NEIGHBORS]; INSTANCE_COUNT],
            socket_storage: [[SocketStorage::default(); SOCKET_COUNT];
                INSTANCE_COUNT],
            ipv6_net: [ipv6_net; INSTANCE_COUNT],
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanEthernet<'a> {
    pub eth: &'a Ethernet,
    pub vid: u16,
}

impl<'a, 'b> smoltcp::phy::Device<'a> for VLanEthernet<'b> {
    type RxToken = VLanRxToken<'a>;
    type TxToken = VLanTxToken<'a>;

    fn receive(&'a mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        if self.eth.can_recv() && self.eth.can_send() {
            Some((
                VLanRxToken(self.eth, self.vid),
                VLanTxToken(self.eth, self.vid),
            ))
        } else {
            None
        }
    }
    fn transmit(&'a mut self) -> Option<Self::TxToken> {
        if self.eth.can_send() {
            Some(VLanTxToken(self.eth, self.vid))
        } else {
            None
        }
    }
    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        self.eth.capabilities()
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanRxToken<'a>(&'a Ethernet, u16);
impl<'a> smoltcp::phy::RxToken for VLanRxToken<'a> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0
            .vlan_try_recv(f, self.1)
            .expect("we checked RX availability earlier")
    }
}

pub struct VLanTxToken<'a>(&'a Ethernet, u16);
impl<'a> smoltcp::phy::TxToken for VLanTxToken<'a> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        len: usize,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0
            .vlan_try_send(len, self.1, f)
            .expect("TX token existed without descriptor available")
    }
}

////////////////////////////////////////////////////////////////////////////////

/// State for the running network server
pub struct ServerImpl<'a> {
    eth: eth::Ethernet,

    socket_handles: [[SocketHandle; SOCKET_COUNT]; INSTANCE_COUNT],
    ifaces: [Interface<'a, VLanEthernet<'a>>; INSTANCE_COUNT],
    bsp: crate::bsp::Bsp,
}

impl<'a> ServerImpl<'a> {
    /// Size of buffer that must be allocated to use `dispatch`.
    pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

    /// Builds a new `ServerImpl`, using the provided storage space.
    pub fn new(
        eth: eth::Ethernet,
        mac: EthernetAddress,
        storage: &'a mut ServerStorage<'a>,
        bsp: crate::bsp::Bsp,
    ) -> Self {
        let mut neighbor_cache_slice = &mut storage.neighbor_cache_storage[..];
        let mut socket_slice = &mut storage.socket_storage[..];
        let mut ip_addr_slice = &storage.ipv6_net[..];

        // Create sockets
        let sockets = generated::construct_sockets();
        let mut socket_handles = [[smoltcp::iface::SocketHandle::default();
            generated::SOCKET_COUNT];
            generated::INSTANCE_COUNT];

        let mut ifaces = [None; generated::INSTANCE_COUNT];

        for (i, (sockets, socket_handles)) in
            sockets.0.into_iter().zip(&mut socket_handles).enumerate()
        {
            let (first, rest) = neighbor_cache_slice.split_at_mut(1);
            neighbor_cache_slice = rest;
            let neighbor_cache =
                smoltcp::iface::NeighborCache::new(&mut first[0][..]);

            let (first, rest) = socket_slice.split_at_mut(1);
            socket_slice = rest;
            let builder = smoltcp::iface::InterfaceBuilder::new(
                handle::EthernetHandle {
                    eth: &eth,

                    #[cfg(feature = "vlan")]
                    vid: generated::VLAN_START + i,
                },
                &mut first[0][..],
            );

            let (first, rest) = ip_addr_slice.split_at_mut(1);
            ip_addr_slice = rest;
            let mut eth = builder
                .hardware_addr(mac.into())
                .neighbor_cache(neighbor_cache)
                .ip_addrs(first)
                .finalize();

            // Associate them with the interface.
            for (socket, h) in
                sockets.into_iter().zip(socket_handles.iter_mut())
            {
                *h = eth.add_socket(socket);
            }
            // Bind sockets to their ports.
            for (&h, &port) in
                socket_handles.iter().zip(&generated::SOCKET_PORTS)
            {
                eth.get_socket::<UdpSocket>(h)
                    .bind(port)
                    .map_err(|_| ())
                    .unwrap();
            }
            ifaces[i] = Some(eth);
        }

        let ifaces = ifaces.map(|e| e.unwrap());
        Self {
            eth,
            socket_handles,
            ifaces,
            bsp,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
