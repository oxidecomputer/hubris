// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation.
//!
//! This module implements a server which listens on a single IPv6 address

use drv_stm32h7_eth as eth;

use crate::bsp_support;
use crate::generated;
use crate::server::{DeviceExt, GenServerImpl, NetServer, Storage};
use core::cell::Cell;
use idol_runtime::{ClientError, RequestError};
use mutable_statics::mutable_statics;
use smoltcp::wire::{EthernetAddress, Ipv6Address};
use task_net_api::{
    LargePayloadBehavior, RecvError, SendError, SocketName, UdpMetadata,
};

/// Grabs references to the server storage arrays.  Can only be called once!
fn claim_server_storage_statics() -> &'static mut [Storage; 1] {
    mutable_statics! {
        static mut STORAGE: [Storage; 1] = [|| Default::default(); _];
    }
}

////////////////////////////////////////////////////////////////////////////////

pub type ServerImpl<'a, B> = GenServerImpl<'a, B, Smol<'a>, 1>;

pub fn new<'a, B>(
    eth: &'a eth::Ethernet,
    ipv6_addr: Ipv6Address,
    mac: EthernetAddress,
    bsp: B,
) -> ServerImpl<'a, B>
where
    B: bsp_support::Bsp,
{
    ServerImpl::new(
        eth,
        ipv6_addr,
        mac,
        bsp,
        claim_server_storage_statics(),
        generated::construct_sockets(),
        |_| Smol::from(eth),
    )
}

impl<B: bsp_support::Bsp> NetServer for GenServerImpl<'_, B, Smol<'_>, 1>
where
    B: bsp_support::Bsp,
{
    type Bsp = B;

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

        // Check that the task owns the socket.
        if generated::SOCKET_OWNERS[socket_index].0.index()
            != msg.sender.index()
        {
            return Err(RecvError::NotYours.into());
        }

        let socket = self
            .get_socket_mut(0, socket_index)
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))?;
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
                    });
                }
                Err(smoltcp::Error::Exhausted) => {
                    return Err(RecvError::QueueEmpty.into());
                }
                Err(_) => {
                    // uhhhh TODO
                    return Err(RecvError::QueueEmpty.into());
                }
            }
        }
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

        let socket = self
            .get_socket_mut(0, socket_index)
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))?;
        match socket.send(payload.len(), metadata.into()) {
            Ok(buf) => {
                payload
                    .read_range(0..payload.len(), buf)
                    .map_err(|_| RequestError::went_away())?;
                self.set_client_waiting_to_send(socket_index, false);
                Ok(())
            }
            Err(smoltcp::Error::Exhausted) => {
                self.set_client_waiting_to_send(socket_index, true);
                Err(SendError::QueueFull.into())
            }
            Err(_e) => {
                // uhhhh TODO
                Err(SendError::Other.into())
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////
// Smoltcp-to-Ethernet bridge for the raw Ethernet device.
//
// We gotta newtype the Ethernet driver since we're not in its crate. (This
// implementation was once in its crate but that became a little gross.)

pub struct Smol<'d> {
    eth: &'d eth::Ethernet,
    mac_rx: Cell<bool>,
}

impl<'d> From<&'d eth::Ethernet> for Smol<'d> {
    fn from(eth: &'d eth::Ethernet) -> Self {
        Self {
            eth,
            mac_rx: Cell::new(false),
        }
    }
}

pub struct OurRxToken<'d>(&'d Smol<'d>);
impl<'d> smoltcp::phy::RxToken for OurRxToken<'d> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0.eth.recv(f)
    }
}

pub struct OurTxToken<'d>(&'d Smol<'d>);
impl<'d> smoltcp::phy::TxToken for OurTxToken<'d> {
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
            .eth
            .try_send(len, f)
            .expect("TX token existed without descriptor available")
    }
}

impl<'d> smoltcp::phy::Device<'d> for Smol<'_> {
    type RxToken = OurRxToken<'d>;
    type TxToken = OurTxToken<'d>;

    fn receive(&'d mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        // Note: smoltcp wants a transmit token every time it receives a
        // packet. This is because it automatically handles stuff like
        // NDP by itself, but means that if the tx queue fills up, we stop
        // being able to receive.
        //
        // Note that the can_recv and can_send checks remain valid because
        // the token mutably borrows the phy.
        if self.eth.can_recv() && self.eth.can_send() {
            // We record this as "data available from the MAC" because it's
            // sufficient to catch the bug we're defending against with the
            // watchdog, even if the IP stack decides not to consume the token
            // for some reason (that'd be a software bug instead).
            self.mac_rx.set(true);

            Some((OurRxToken(self), OurTxToken(self)))
        } else {
            None
        }
    }

    fn transmit(&'d mut self) -> Option<Self::TxToken> {
        if self.eth.can_send() {
            Some(OurTxToken(self))
        } else {
            None
        }
    }

    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        crate::ethernet_capabilities(self.eth)
    }
}

impl DeviceExt for Smol<'_> {
    fn read_and_clear_activity_flag(&self) -> bool {
        self.mac_rx.take()
    }
}
