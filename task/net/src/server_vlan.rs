// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation with VLAN support
//!
//! This module implements a server which listens on multiple (incrementing)
//! IPv6 addresses and supports some number of VLANs.

use drv_stm32h7_eth as eth;

use core::cell::Cell;
use idol_runtime::{ClientError, RequestError};
use mutable_statics::mutable_statics;
use smoltcp::wire::{EthernetAddress, Ipv6Address};
use task_net_api::{
    LargePayloadBehavior, RecvError, SendError, SocketName, UdpMetadata,
};

use crate::bsp_support;
use crate::generated::{self, VLAN_COUNT, VLAN_RANGE};
use crate::server::{DeviceExt, GenServerImpl, NetServer, Storage};

/// Grabs references to the server storage arrays.  Can only be called once!
fn claim_server_storage_statics() -> &'static mut [Storage; VLAN_COUNT] {
    mutable_statics! {
        static mut STORAGE: [Storage; VLAN_COUNT] = [|| Default::default(); _];
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanEthernet<'a> {
    pub eth: &'a eth::Ethernet,
    pub vid: u16,
    mac_rx: Cell<bool>,
}

impl<'a, 'b> smoltcp::phy::Device<'a> for VLanEthernet<'b> {
    type RxToken = VLanRxToken<'a>;
    type TxToken = VLanTxToken<'a>;

    fn receive(&'a mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        if self.eth.vlan_can_recv(self.vid, VLAN_RANGE) && self.eth.can_send() {
            self.mac_rx.set(true);
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
        crate::ethernet_capabilities(self.eth)
    }
}

impl DeviceExt for VLanEthernet<'_> {
    fn read_and_clear_activity_flag(&self) -> bool {
        self.mac_rx.take()
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

pub type ServerImpl<'a, B> = GenServerImpl<'a, B, VLanEthernet<'a>, VLAN_COUNT>;

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
        |i| VLanEthernet {
            eth,
            vid: generated::VLAN_RANGE.start + i as u16,
            mac_rx: Cell::new(false),
        },
    )
}

/// Implementation of the Net Idol interface.
impl<B> NetServer for ServerImpl<'_, B>
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
