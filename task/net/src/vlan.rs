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
use smoltcp::wire::{
    EthernetAddress, IpAddress, IpCidr, Ipv6Address, Ipv6Cidr,
};
use task_net_api::{NetError, SocketName, UdpMetadata};
use userlib::{sys_post, sys_refresh_task_id};

use crate::generated::{self, SOCKET_COUNT, VLAN_COUNT};
use crate::{idl, ETH_IRQ, NEIGHBORS, WAKE_IRQ};

type NeighborStorage = Option<(IpAddress, Neighbor)>;

/// Storage required to run a single [ServerImpl]. This should be allocated
/// on the stack and passed into the constructor for the [ServerImpl].
pub struct ServerStorage<'a> {
    pub eth: eth::Ethernet,
    neighbor_cache_storage: [[NeighborStorage; NEIGHBORS]; VLAN_COUNT],
    socket_storage: [[SocketStorage<'a>; SOCKET_COUNT]; VLAN_COUNT],
    ipv6_net: [IpCidr; VLAN_COUNT],
}

impl<'a> ServerStorage<'a> {
    pub fn new(eth: eth::Ethernet) -> Self {
        Self {
            eth,
            neighbor_cache_storage: Default::default(),
            socket_storage: Default::default(),
            ipv6_net: [Ipv6Cidr::default().into(); VLAN_COUNT],
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanEthernet<'a> {
    pub eth: &'a eth::Ethernet,
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

pub struct VLanRxToken<'a>(&'a eth::Ethernet, u16);
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
            .vlan_try_recv(self.1, f)
            .expect("we checked RX availability earlier")
    }
}

pub struct VLanTxToken<'a>(&'a eth::Ethernet, u16);
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
    eth: &'a eth::Ethernet,

    socket_handles: [[SocketHandle; SOCKET_COUNT]; VLAN_COUNT],
    ifaces: [Interface<'a, VLanEthernet<'a>>; VLAN_COUNT],
    bsp: crate::bsp::Bsp,
}

impl<'a> ServerImpl<'a> {
    /// Size of buffer that must be allocated to use `dispatch`.
    pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

    /// Builds a new `ServerImpl`, using the provided storage space.
    pub fn new(
        storage: &'a mut ServerStorage<'a>,
        ipv6_addr: Ipv6Address,
        mac: EthernetAddress,
        bsp: crate::bsp::Bsp,
    ) -> Self {
        let mut neighbor_cache_slice = &mut storage.neighbor_cache_storage[..];
        let mut socket_slice = &mut storage.socket_storage[..];
        let mut ip_addr_slice = &mut storage.ipv6_net[..];

        // Create sockets
        let sockets = generated::construct_sockets();
        let mut socket_handles = [[smoltcp::iface::SocketHandle::default();
            generated::SOCKET_COUNT];
            generated::VLAN_COUNT];

        let mut ifaces: [Option<Interface<'a, VLanEthernet<'a>>>; VLAN_COUNT] =
            Default::default();

        for (i, (sockets, socket_handles)) in
            sockets.0.into_iter().zip(&mut socket_handles).enumerate()
        {
            ip_addr_slice[0] = Ipv6Cidr::new(ipv6_addr, 64).into();
            let (first, rest) = neighbor_cache_slice.split_at_mut(1);
            neighbor_cache_slice = rest;
            let neighbor_cache =
                smoltcp::iface::NeighborCache::new(&mut first[0][..]);

            let (first, rest) = socket_slice.split_at_mut(1);
            socket_slice = rest;
            let builder = smoltcp::iface::InterfaceBuilder::new(
                VLanEthernet {
                    eth: &storage.eth,
                    vid: (generated::VLAN_START + i).try_into().unwrap(),
                },
                &mut first[0][..],
            );

            let (first, rest) = ip_addr_slice.split_at_mut(1);
            ip_addr_slice = rest;
            let mut iface = builder
                .hardware_addr(mac.into())
                .neighbor_cache(neighbor_cache)
                .ip_addrs(first)
                .finalize();

            // Associate them with the interface.
            for (socket, h) in
                sockets.into_iter().zip(socket_handles.iter_mut())
            {
                *h = iface.add_socket(socket);
            }
            // Bind sockets to their ports.
            for (&h, &port) in
                socket_handles.iter().zip(&generated::SOCKET_PORTS)
            {
                iface
                    .get_socket::<UdpSocket>(h)
                    .bind(port)
                    .map_err(|_| ())
                    .unwrap();
            }
            ifaces[i] = Some(iface);
        }

        let ifaces = ifaces.map(|e| e.unwrap());
        Self {
            eth: &storage.eth,
            socket_handles,
            ifaces,
            bsp,
        }
    }

    pub fn poll(&mut self, t: u64) -> smoltcp::Result<bool> {
        unimplemented!()
    }

    /// Iterate over sockets, waking any that can do work.
    pub fn wake_sockets(&mut self) {
        unimplemented!()
    }

    pub fn wake(&self) {
        self.bsp.wake(&self.eth)
    }
}

/// Implementation of the Net Idol interface.
impl idl::InOrderNetImpl for ServerImpl<'_> {
    /// Requests that a packet waiting in the rx queue of `socket` be delivered
    /// into loaned memory at `payload`.
    ///
    /// If a packet is available and fits, copies it into `payload` and returns
    /// its `UdpMetadata`. Otherwise, leaves `payload` untouched and returns an
    /// error.
    fn recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<NetError>> {
        unimplemented!()
    }
    /// Requests to copy a packet into the tx queue of socket `socket`,
    /// described by `metadata` and containing the bytes loaned in `payload`.
    fn send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<NetError>> {
        unimplemented!()
    }

    fn smi_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
    ) -> Result<u16, RequestError<NetError>> {
        // TODO: this should not be open to all callers!
        Ok(self.eth.smi_read(phy, register))
    }

    fn smi_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
        value: u16,
    ) -> Result<(), RequestError<NetError>> {
        // TODO: this should not be open to all callers!
        Ok(self.eth.smi_write(phy, register, value))
    }
}

impl NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        // We're always listening for our interrupt or the wake (timer) irq
        ETH_IRQ | WAKE_IRQ
    }

    fn handle_notification(&mut self, bits: u32) {
        // Interrupt dispatch.
        if bits & ETH_IRQ != 0 {
            self.eth.on_interrupt();
            userlib::sys_irq_control(ETH_IRQ, true);
        }
        // The wake IRQ is handled in the main `net` loop
    }
}
