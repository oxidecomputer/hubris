// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation.
//!
//! This module implements a server which listens on a single IPv6 address

use drv_stm32h7_eth as eth;

use crate::bsp_support;
use crate::generated;
use crate::{
    server::{DeviceExt, GenServerImpl, Storage},
    MacAddressBlock,
};
use mutable_statics::mutable_statics;
use task_net_api::UdpMetadata;

/// Grabs references to the server storage arrays.  Can only be called once!
fn claim_server_storage_statics() -> &'static mut [Storage; 1] {
    mutable_statics! {
        static mut STORAGE: [Storage; 1] = [Default::default; _];
    }
}

////////////////////////////////////////////////////////////////////////////////

pub type ServerImpl<'a, B> = GenServerImpl<'a, B, Smol<'a>>;

pub fn new<B>(
    eth: &eth::Ethernet,
    mac: MacAddressBlock,
    bsp: B,
) -> ServerImpl<'_, B>
where
    B: bsp_support::Bsp,
{
    ServerImpl::new(
        eth,
        mac,
        bsp,
        claim_server_storage_statics(),
        generated::construct_sockets(),
        |_| Smol::from(eth),
    )
}

///////////////////////////////////////////////////////////////////////////
// Smoltcp-to-Ethernet bridge for the raw Ethernet device.
//
// We gotta newtype the Ethernet driver since we're not in its crate. (This
// implementation was once in its crate but that became a little gross.)

pub struct Smol<'d> {
    eth: &'d eth::Ethernet,
}

impl<'d> From<&'d eth::Ethernet> for Smol<'d> {
    fn from(eth: &'d eth::Ethernet) -> Self {
        Self { eth }
    }
}

pub struct OurRxToken<'d>(&'d eth::Ethernet);
impl<'d> smoltcp::phy::RxToken for OurRxToken<'d> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0.recv(f)
    }
}

pub struct OurTxToken<'d>(&'d eth::Ethernet);
impl<'d> smoltcp::phy::TxToken for OurTxToken<'d> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0
            .try_send(len, f)
            .expect("TX token existed without descriptor available")
    }
}

impl<'a> smoltcp::phy::Device for Smol<'a> {
    type RxToken<'b>
        = OurRxToken<'b>
    where
        Self: 'b;
    type TxToken<'b>
        = OurTxToken<'b>
    where
        Self: 'b;

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'a>, Self::TxToken<'a>)> {
        // Note: smoltcp wants a transmit token every time it receives a
        // packet. This is because it automatically handles stuff like
        // NDP by itself, but means that if the tx queue fills up, we stop
        // being able to receive.
        //
        // Note that the can_recv and can_send checks remain valid because
        // the token mutably borrows the phy.
        if self.eth.can_recv() && self.eth.can_send() {
            Some((OurRxToken(self.eth), OurTxToken(self.eth)))
        } else {
            None
        }
    }

    fn transmit(
        &mut self,
        _i: smoltcp::time::Instant,
    ) -> Option<Self::TxToken<'a>> {
        if self.eth.can_send() {
            Some(OurTxToken(self.eth))
        } else {
            None
        }
    }

    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        crate::ethernet_capabilities(self.eth)
    }
}

impl DeviceExt for Smol<'_> {
    fn make_meta(
        &self,
        port: u16,
        size: usize,
        addr: task_net_api::Address,
    ) -> UdpMetadata {
        UdpMetadata {
            port,
            size: size as u32,
            addr,
        }
    }
}
