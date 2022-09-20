// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation with VLAN support
//!
//! This module implements a server which listens on multiple (incrementing)
//! IPv6 addresses and supports some number of VLANs.

use drv_stm32h7_eth as eth;

use idol_runtime::{ClientError, NotificationHandler, RequestError};
use mutable_statics::mutable_statics;
use smoltcp::iface::{Interface, Neighbor, SocketHandle, SocketStorage};
use smoltcp::socket::UdpSocket;
use smoltcp::wire::{
    EthernetAddress, IpAddress, IpCidr, Ipv6Address, Ipv6Cidr,
};
use task_net_api::{
    LargePayloadBehavior, RecvError, SendError, SocketName, UdpMetadata,
};
use userlib::{sys_post, sys_refresh_task_id};

use crate::generated::{self, SOCKET_COUNT, VLAN_COUNT, VLAN_RANGE};
use crate::server::NetServer;
use crate::{idl, ETH_IRQ, NEIGHBORS, WAKE_IRQ};

type NeighborStorage = Option<(IpAddress, Neighbor)>;

/// Grabs references to the server storage arrays.  Can only be called once!
pub fn claim_server_storage_statics() -> (
    &'static mut [[NeighborStorage; NEIGHBORS]; VLAN_COUNT],
    &'static mut [[SocketStorage<'static>; SOCKET_COUNT]; VLAN_COUNT],
    &'static mut [IpCidr; VLAN_COUNT],
) {
    mutable_statics! {
        static mut NEIGHBOR_CACHE_STORAGE:
            [[NeighborStorage; NEIGHBORS]; VLAN_COUNT] =
            [Default::default(); _];
        static mut SOCKET_STORAGE:
            [[SocketStorage<'static>; SOCKET_COUNT]; VLAN_COUNT] =
            [Default::default(); _];
        static mut IPV6_NET: [IpCidr; VLAN_COUNT] =
            [Ipv6Cidr::default().into(); _];
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
    client_waiting_to_send: [bool; SOCKET_COUNT],
    ifaces: [Interface<'static, VLanEthernet<'a>>; VLAN_COUNT],
    bsp: crate::bsp::Bsp,

    mac: EthernetAddress,
}

impl<'a> ServerImpl<'a> {
    /// Size of buffer that must be allocated to use `dispatch`.
    pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

    /// Builds a new `ServerImpl`, using the provided storage space.
    pub fn new(
        eth: &'a eth::Ethernet,
        mut ipv6_addr: Ipv6Address,
        mut mac: EthernetAddress,
        bsp: crate::bsp::Bsp,
    ) -> Self {
        // Local storage; this will end up owned by the returned ServerImpl.
        let mut socket_handles = [[Default::default(); generated::SOCKET_COUNT];
            generated::VLAN_COUNT];
        let mut ifaces: [Option<Interface<'_, VLanEthernet<'_>>>; VLAN_COUNT] =
            Default::default();

        let (n, s, i) = claim_server_storage_statics();

        // We're iterating over a bunch of things together.  The standard
        // library doesn't have a great multi-element zip, so we'll just
        // manually use mutable iterators.
        let mut neighbor_cache_iter = n.iter_mut();
        let mut socket_storage_iter = s.iter_mut();
        let mut socket_handles_iter = socket_handles.iter_mut();
        let mut vid_iter = generated::VLAN_RANGE;
        let mut ifaces_iter = ifaces.iter_mut();
        let mut ip_addr_iter = i.chunks_mut(1);

        // Create a VLAN_COUNT x SOCKET_COUNT nested array of sockets
        let sockets = generated::construct_sockets();
        assert_eq!(sockets.0.len(), VLAN_COUNT);

        let start_mac = mac;
        for sockets in sockets.0.into_iter() {
            let neighbor_cache_storage = neighbor_cache_iter.next().unwrap();
            let neighbor_cache = smoltcp::iface::NeighborCache::new(
                &mut neighbor_cache_storage[..],
            );

            let socket_storage = socket_storage_iter.next().unwrap();
            let builder = smoltcp::iface::InterfaceBuilder::new(
                VLanEthernet {
                    eth,
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
                    .get_socket::<UdpSocket<'_>>(h)
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
            eth,
            client_waiting_to_send: [false; SOCKET_COUNT],
            socket_handles,
            ifaces,
            bsp,
            mac: start_mac,
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
            if (0..VLAN_COUNT).any(|v| {
                let want_to_send = self.client_waiting_to_send[i];
                let socket = self.get_socket_mut(i, v).unwrap();
                socket.can_recv() || (want_to_send && socket.can_send())
            }) {
                let (task_id, notification) = generated::SOCKET_OWNERS[i];
                let task_id = sys_refresh_task_id(task_id);
                sys_post(task_id, notification);
            }
        }
    }

    pub fn wake(&self) {
        self.bsp.wake(self.eth)
    }

    fn get_handle(
        &self,
        index: usize,
        vlan_index: usize,
    ) -> Result<SocketHandle, ClientError> {
        self.socket_handles
            .get(vlan_index)
            .ok_or(ClientError::BadMessageContents)
            .and_then(|s| {
                s.get(index).cloned().ok_or(ClientError::BadMessageContents)
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
    ) -> Result<&mut UdpSocket<'static>, ClientError> {
        Ok(self.ifaces[vlan_index]
            .get_socket::<UdpSocket<'_>>(self.get_handle(index, vlan_index)?))
    }
}

/// Implementation of the Net Idol interface.
impl NetServer for ServerImpl<'_> {
    /// Requests that a packet waiting in the rx queue of `socket` be delivered
    /// into loaned memory at `payload`.
    ///
    /// If a packet is available and fits, copies it into `payload` and returns
    /// its `UdpMetadata`. Otherwise, leaves `payload` untouched and returns an
    /// error.
    fn net_recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        large_payload_behavior: LargePayloadBehavior,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<RecvError>> {
        let socket_index = socket as usize;

        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(RecvError::NotYours.into());
        }

        // Iterate over all of the per-VLAN sockets, returning the first
        // available packet with a bonus `vid` tag attached in the metadata.
        for (i, vid) in VLAN_RANGE.enumerate() {
            let socket = self
                .get_socket_mut(socket_index, i)
                .map_err(RequestError::Fail)?;
            loop {
                match socket.recv() {
                    Ok((body, endp)) => {
                        if payload.len() < body.len() {
                            match large_payload_behavior {
                                LargePayloadBehavior::Discard => continue,
                                // If we add a `::Fail` case, we will need to
                                // allow for caller retries (possibly by peeking
                                // on the socket instead of recving)
                            }
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
                        // Move on to next vid
                        break;
                    }
                    Err(_) => {
                        // uhhhh TODO
                        // (move on to next vid in the meantime)
                        break;
                    }
                }
            }
        }
        Err(RecvError::QueueEmpty.into())
    }

    /// Requests to copy a packet into the tx queue of socket `socket`,
    /// described by `metadata` and containing the bytes loaned in `payload`.
    fn net_send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<SendError>> {
        let socket_index = socket as usize;
        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(SendError::NotYours.into());
        }

        // Convert from absolute VID to an index in our VLAN array
        if !VLAN_RANGE.contains(&metadata.vid) {
            return Err(SendError::InvalidVLan.into());
        }
        let vlan_index = metadata.vid - VLAN_RANGE.start;

        let socket = self
            .get_socket_mut(socket_index, vlan_index as usize)
            .map_err(RequestError::Fail)?;
        match socket.send(payload.len(), metadata.into()) {
            Ok(buf) => {
                payload
                    .read_range(0..payload.len(), buf)
                    .map_err(|_| RequestError::went_away())?;
                self.client_waiting_to_send[socket_index] = false;
                Ok(())
            }
            Err(smoltcp::Error::Exhausted) => {
                self.client_waiting_to_send[socket_index] = true;
                Err(SendError::QueueFull.into())
            }
            Err(_e) => {
                // uhhhh TODO
                Err(SendError::Other.into())
            }
        }
    }

    fn eth_bsp(&mut self) -> (&eth::Ethernet, &mut crate::bsp::Bsp) {
        (self.eth, &mut self.bsp)
    }

    fn base_mac_address(&self) -> &EthernetAddress {
        &self.mac
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
