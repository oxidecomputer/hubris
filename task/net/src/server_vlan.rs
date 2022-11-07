// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation with VLAN support
//!
//! This module implements a server which listens on multiple (incrementing)
//! IPv6 addresses and supports some number of VLANs.

use drv_stm32h7_eth as eth;

use core::cell::Cell;
use mutable_statics::mutable_statics;
use task_net_api::UdpMetadata;

use crate::bsp_support;
use crate::generated::{self, VLAN_COUNT, VLAN_RANGE};
use crate::{
    server::{DeviceExt, GenServerImpl, Storage},
    MacAddressBlock,
};

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
            vid: self.vid,
        }
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
    mac: MacAddressBlock,
    bsp: B,
) -> ServerImpl<'a, B>
where
    B: bsp_support::Bsp,
{
    ServerImpl::new(
        eth,
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
