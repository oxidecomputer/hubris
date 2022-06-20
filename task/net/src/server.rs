// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation.
//!
//! This module implements a server which listens on a single IPv6 address

use drv_stm32h7_eth as eth;

use idol_runtime::{ClientError, NotificationHandler, RequestError};
use smoltcp::iface::{Interface, Neighbor, SocketHandle, SocketStorage};
use smoltcp::socket::UdpSocket;
use smoltcp::wire::{
    EthernetAddress, IpAddress, IpCidr, Ipv6Address, Ipv6Cidr,
};
use task_net_api::{NetError, SocketName, UdpMetadata};
use userlib::{sys_post, sys_refresh_task_id};

use crate::generated::{self, SOCKET_COUNT};
use crate::{idl, ETH_IRQ, NEIGHBORS, WAKE_IRQ};

/// Storage required to run a single [ServerImpl]. This should be allocated
/// on the stack and passed into the constructor for the [ServerImpl].
pub struct ServerStorage<'a> {
    pub eth: eth::Ethernet,

    neighbor_cache_storage: [Option<(IpAddress, Neighbor)>; NEIGHBORS],
    socket_storage: [SocketStorage<'a>; SOCKET_COUNT],
    ipv6_net: [IpCidr; 1],
}

impl<'a> ServerStorage<'a> {
    pub fn new(eth: eth::Ethernet) -> Self {
        Self {
            eth,
            neighbor_cache_storage: [None; NEIGHBORS],
            socket_storage: Default::default(),
            ipv6_net: [Ipv6Cidr::default().into()],
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// State for the running network server.
pub struct ServerImpl<'a> {
    socket_handles: [SocketHandle; SOCKET_COUNT],
    iface: Interface<'a, &'a eth::Ethernet>,
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
        storage.ipv6_net[0] = Ipv6Cidr::new(ipv6_addr, 64).into();
        let neighbor_cache = smoltcp::iface::NeighborCache::new(
            &mut storage.neighbor_cache_storage[..],
        );
        let mut iface = smoltcp::iface::InterfaceBuilder::new(
            &storage.eth,
            &mut storage.socket_storage[..],
        )
        .hardware_addr(mac.into())
        .neighbor_cache(neighbor_cache)
        .ip_addrs(&mut storage.ipv6_net[..])
        .finalize();

        // Create sockets and associate them with the interface.
        let sockets = generated::construct_sockets();
        let mut socket_handles = [None; generated::SOCKET_COUNT];
        for (socket, h) in sockets.0.into_iter().zip(&mut socket_handles) {
            *h = Some(iface.add_socket(socket));
        }
        let socket_handles = socket_handles.map(|h| h.unwrap());
        // Bind sockets to their ports.
        for (&h, &port) in socket_handles.iter().zip(&generated::SOCKET_PORTS) {
            iface
                .get_socket::<UdpSocket>(h)
                .bind((ipv6_addr, port))
                .map_err(|_| ())
                .unwrap();
        }

        Self {
            socket_handles,
            iface,
            bsp,
        }
    }

    /// Calls `smoltcp`'s internal poll function on our interface
    pub fn poll(&mut self, t: u64) -> smoltcp::Result<bool> {
        self.iface
            .poll(smoltcp::time::Instant::from_millis(t as i64))
    }

    /// Iterate over sockets, waking any that can do work.
    pub fn wake_sockets(&mut self) {
        // There's something to do! Iterate over sockets looking for work.
        // TODO making every packet O(n) in the number of sockets is super
        // lame; provide a Waker to fix this.
        for i in 0..SOCKET_COUNT {
            if self.get_socket_mut(i).unwrap().can_recv() {
                // Make sure the owner knows about this. This can
                // technically cause spurious wakeups if the owner is
                // already waiting in our incoming queue to recv. Maybe we
                // fix this later.
                let (task_id, notification) = generated::SOCKET_OWNERS[i];
                let task_id = sys_refresh_task_id(task_id);
                sys_post(task_id, notification);
            }
        }
    }

    /// Gets the socket handle for socket `index`. If `index` is out of range,
    /// returns `BadMessage`.
    ///
    /// You often want `get_socket_mut` instead of this, but since it claims
    /// `self` mutably, it is sometimes useful to inline it by calling this
    /// followed by `eth.get_socket`.
    fn get_handle(
        &self,
        index: usize,
    ) -> Result<SocketHandle, RequestError<NetError>> {
        self.socket_handles
            .get(index)
            .cloned()
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))
    }

    /// Gets the socket `index`. If `index` is out of range, returns
    /// `BadMessage`.
    ///
    /// Sockets are currently assumed to be UDP.
    fn get_socket_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut UdpSocket<'a>, RequestError<NetError>> {
        Ok(self.iface.get_socket::<UdpSocket>(self.get_handle(index)?))
    }

    /// Calls the `wake` function on the BSP, which handles things like
    /// periodic logging and monitoring of ports.
    pub fn wake(&self) {
        self.bsp.wake(self.iface.device());
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

        // Check that the task owns the socket.
        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(NetError::NotYours.into());
        }

        let socket = self.get_socket_mut(socket_index)?;
        match socket.recv() {
            Ok((body, endp)) => {
                if payload.len() < body.len() {
                    return Err(RequestError::Fail(ClientError::BadLease));
                }
                payload
                    .write_range(0..body.len(), body)
                    .map_err(|_| RequestError::went_away())?;

                Ok(UdpMetadata {
                    port: endp.port,
                    size: body.len() as u32,
                    addr: endp.addr.try_into().map_err(|_| ()).unwrap(),
                })
            }
            Err(smoltcp::Error::Exhausted) => Err(NetError::QueueEmpty.into()),
            Err(_) => {
                // uhhhh TODO
                Err(NetError::QueueEmpty.into())
            }
        }
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

        let socket = self.get_socket_mut(socket_index)?;
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
        Ok(self.iface.device().smi_read(phy, register))
    }

    fn smi_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
        value: u16,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        // TODO: this should not be open to all callers!
        Ok(self.iface.device().smi_write(phy, register, value))
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
            self.iface.device().on_interrupt();
            userlib::sys_irq_control(ETH_IRQ, true);
        }
        // The wake IRQ is handled in the main `net` loop
    }
}
