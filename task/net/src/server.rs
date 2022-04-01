// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation.

use idol_runtime::{ClientError, NotificationHandler, RequestError};
use smoltcp::iface::{Interface, SocketHandle};
use smoltcp::socket::UdpSocket;
use task_net_api::{NetError, SocketName, UdpMetadata};

use crate::generated;
use crate::{ETH_IRQ, WAKE_IRQ};

/// State for the running network server.
pub struct ServerImpl<'a> {
    socket_handles: [SocketHandle; generated::SOCKET_COUNT],
    eth: Interface<'a, drv_stm32h7_eth::Ethernet>,
    bsp: crate::bsp::Bsp,
}

impl<'a> ServerImpl<'a> {
    /// Size of buffer that must be allocated to use `dispatch`.
    pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

    /// Moves bits required by the server into a new `ServerImpl`.
    pub fn new(
        socket_handles: [SocketHandle; generated::SOCKET_COUNT],
        eth: Interface<'a, drv_stm32h7_eth::Ethernet>,
        bsp: crate::bsp::Bsp,
    ) -> Self {
        Self {
            socket_handles,
            eth,
            bsp,
        }
    }

    /// Borrows a direct reference to the `smoltcp` `Interface` inside the
    /// server. This is exposed for use by the driver loop in main.
    pub fn interface_mut(
        &mut self,
    ) -> &mut Interface<'a, drv_stm32h7_eth::Ethernet> {
        &mut self.eth
    }
}

impl<'a> ServerImpl<'a> {
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
    pub fn get_socket_mut(
        &mut self,
        index: usize,
    ) -> Result<&mut UdpSocket<'a>, RequestError<NetError>> {
        Ok(self.eth.get_socket::<UdpSocket>(self.get_handle(index)?))
    }

    /// Calls the `wake` function on the BSP, which handles things like
    /// periodic logging and monitoring of ports.
    pub fn wake(&mut self) {
        self.bsp.wake(self.eth.device_mut());
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
    ) -> Result<u16, RequestError<NetError>> {
        // TODO: this should not be open to all callers!
        Ok(self.eth.device_mut().smi_read(phy, register))
    }

    fn smi_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
        value: u16,
    ) -> Result<(), RequestError<NetError>> {
        // TODO: this should not be open to all callers!
        Ok(self.eth.device_mut().smi_write(phy, register, value))
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
            self.eth.device_mut().on_interrupt();
            userlib::sys_irq_control(ETH_IRQ, true);
        }
        // The wake IRQ is handled in the main `net` loop
    }
}

mod idl {
    use task_net_api::{NetError, SocketName, UdpMetadata};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
