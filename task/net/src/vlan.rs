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

use crate::generated::{self, SOCKET_COUNT, VLAN_COUNT, VLAN_RANGE};
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
        if self.eth.vlan_can_recv(self.vid, VLAN_RANGE) && self.eth.can_send() {
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
        self.0.vlan_recv(self.1, f)
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
        mut ipv6_addr: Ipv6Address,
        mut mac: EthernetAddress,
        bsp: crate::bsp::Bsp,
    ) -> Self {
        // Local storage; this will end up owned by the returned ServerImpl.
        let mut socket_handles = [[Default::default(); generated::SOCKET_COUNT];
            generated::VLAN_COUNT];
        let mut ifaces: [Option<Interface<'a, VLanEthernet<'a>>>; VLAN_COUNT] =
            Default::default();

        // We're iterating over a bunch of things together.  The standard
        // library doesn't have a great multi-element zip, so we'll just
        // manually use mutable iterators.
        let mut neighbor_cache_iter = storage.neighbor_cache_storage.iter_mut();
        let mut socket_storage_iter = storage.socket_storage.iter_mut();
        let mut socket_handles_iter = socket_handles.iter_mut();
        let mut vid_iter = generated::VLAN_RANGE;
        let mut ifaces_iter = ifaces.iter_mut();
        let mut ip_addr_iter = storage.ipv6_net.chunks_mut(1);

        // Create a VLAN_COUNT x SOCKET_COUNT nested array of sockets
        let sockets = generated::construct_sockets();
        assert_eq!(sockets.0.len(), VLAN_COUNT);

        for sockets in sockets.0.into_iter() {
            let neighbor_cache_storage = neighbor_cache_iter.next().unwrap();
            let neighbor_cache = smoltcp::iface::NeighborCache::new(
                &mut neighbor_cache_storage[..],
            );

            let socket_storage = socket_storage_iter.next().unwrap();
            let builder = smoltcp::iface::InterfaceBuilder::new(
                VLanEthernet {
                    eth: &storage.eth,
                    vid: vid_iter.next().unwrap(),
                },
                &mut socket_storage[..],
            );

            let ipv6_net = ip_addr_iter.next().unwrap();
            ipv6_net[0] = Ipv6Cidr::new(ipv6_addr, 64).into();
            let mut iface = builder
                .hardware_addr(mac.into())
                .neighbor_cache(neighbor_cache)
                .ip_addrs(ipv6_net)
                .finalize();

            // Associate sockets with this interface.
            let socket_handles = socket_handles_iter.next().unwrap();
            assert_eq!(sockets.len(), SOCKET_COUNT);
            for (s, h) in sockets.into_iter().zip(&mut socket_handles[..]) {
                *h = iface.add_socket(s);
            }
            // Bind sockets to their ports.
            assert_eq!(socket_handles.len(), SOCKET_COUNT);
            assert_eq!(generated::SOCKET_PORTS.len(), SOCKET_COUNT);
            for (&h, &port) in
                socket_handles.iter().zip(&generated::SOCKET_PORTS)
            {
                iface
                    .get_socket::<UdpSocket>(h)
                    .bind((ipv6_addr, port))
                    .map_err(|_| ())
                    .unwrap();
            }
            *ifaces_iter.next().unwrap() = Some(iface);

            // Increment the MAC and IP addresses so that each VLAN has
            // a unique address.
            ipv6_addr.0[15] += 1;
            mac.0[5] += 1;
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
        let t = smoltcp::time::Instant::from_millis(t as i64);
        let mut any_activity = false;
        for iface in &mut self.ifaces {
            any_activity |= iface.poll(t)?;
        }
        Ok(any_activity)
    }

    /// Iterate over sockets, waking any that can do work.  A task can do work
    /// if all of the (internal) VLAN sockets can receive a packet, since
    /// we don't know which VLAN it will write to.
    pub fn wake_sockets(&mut self) {
        for i in 0..SOCKET_COUNT {
            if (0..VLAN_COUNT)
                .any(|v| self.get_socket_mut(i, v).unwrap().can_recv())
            {
                let (task_id, notification) = generated::SOCKET_OWNERS[i];
                let task_id = sys_refresh_task_id(task_id);
                sys_post(task_id, notification);
            }
        }
    }

    pub fn wake(&self) {
        self.bsp.wake(&self.eth)
    }

    fn get_handle(
        &self,
        index: usize,
        vlan_index: usize,
    ) -> Result<SocketHandle, RequestError<NetError>> {
        self.socket_handles
            .get(vlan_index)
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
            .and_then(|s| {
                s.get(index)
                    .cloned()
                    .ok_or(RequestError::Fail(ClientError::BadMessageContents))
            })
    }

    /// Gets the socket `index`. If `index` is out of range, returns
    /// `BadMessage`. Panics if `vlan_index` is out of range, which should
    /// never happen (because messages with invalid VIDs are dropped in
    /// RxRing).
    ///
    /// Sockets are currently assumed to be UDP.
    fn get_socket_mut(
        &mut self,
        index: usize,
        vlan_index: usize,
    ) -> Result<&mut UdpSocket<'a>, RequestError<NetError>> {
        Ok(self.ifaces[vlan_index]
            .get_socket::<UdpSocket>(self.get_handle(index, vlan_index)?))
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
        let socket_index = socket as usize;

        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(NetError::NotYours.into());
        }

        // Iterate over all of the per-VLAN sockets, returning the first
        // available packet with a bonus `vid` tag attached in the metadata.
        for (i, vid) in VLAN_RANGE.enumerate() {
            let socket = self.get_socket_mut(socket_index, i)?;
            match socket.recv() {
                Ok((body, endp)) => {
                    if payload.len() < body.len() {
                        return Err(RequestError::Fail(ClientError::BadLease));
                    }
                    payload
                        .write_range(0..body.len(), body)
                        .map_err(|_| RequestError::went_away())?;

                    return Ok(UdpMetadata {
                        port: endp.port,
                        size: body.len() as u32,
                        addr: endp.addr.try_into().map_err(|_| ()).unwrap(),
                        vid,
                    });
                }
                Err(smoltcp::Error::Exhausted) => {
                    // Keep iterating
                }
                Err(_) => {
                    // uhhhh TODO
                    // (keep iterating in the meantime)
                }
            }
        }
        Err(NetError::QueueEmpty.into())
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
        let socket_index = socket as usize;
        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(NetError::NotYours.into());
        }

        // Convert from absolute VID to an index in our VLAN array
        if !VLAN_RANGE.contains(&metadata.vid) {
            return Err(NetError::InvalidVLan.into());
        }
        let vlan_index = metadata.vid - VLAN_RANGE.start;

        let socket = self.get_socket_mut(socket_index, vlan_index as usize)?;
        match socket.send(payload.len(), metadata.into()) {
            Ok(buf) => {
                payload
                    .read_range(0..payload.len(), buf)
                    .map_err(|_| RequestError::went_away())?;
                Ok(())
            }
            Err(smoltcp::Error::Exhausted) => {
                // TODO this is not quite right
                Err(NetError::QueueEmpty.into())
            }
            Err(_e) => {
                // uhhhh TODO
                // TODO this is not quite right
                Err(NetError::QueueEmpty.into())
            }
        }
    }

    fn smi_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
    ) -> Result<u16, idol_runtime::RequestError<core::convert::Infallible>>
    {
        // TODO: this should not be open to all callers!
        Ok(self.eth.smi_read(phy, register))
    }

    fn smi_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
        value: u16,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
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
