// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Network IPC server implementation with VLAN support
//!
//! This module implements a server which listens on multiple (incrementing)
//! IPv6 addresses and supports some number of VLANs.

use drv_stm32h7_eth as eth;

use enum_map::Enum;
use mutable_statics::mutable_statics;
use task_net_api::{UdpMetadata, VLanId, VLAN_VIDS};

use crate::bsp_support;
use crate::generated::{self};
use crate::{
    server::{DeviceExt, GenServerImpl, Storage},
    MacAddressBlock,
};

/// Grabs references to the server storage arrays.  Can only be called once!
fn claim_server_storage_statics() -> &'static mut [Storage; VLanId::LENGTH] {
    mutable_statics! {
        static mut STORAGE: [Storage; VLanId::LENGTH] = [Default::default; _];
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanEthernet<'a> {
    pub eth: &'a eth::Ethernet,
    pub vid: VLanId,
}

impl<'a> smoltcp::phy::Device for VLanEthernet<'a> {
    type RxToken<'b>
        = VLanRxToken<'a>
    where
        Self: 'b;
    type TxToken<'b>
        = VLanTxToken<'a>
    where
        Self: 'b;

    fn receive(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<(Self::RxToken<'a>, Self::TxToken<'a>)> {
        let vid = self.vid.cfg().vid;
        if self.eth.vlan_can_recv(vid, &VLAN_VIDS) && self.eth.can_send() {
            Some((VLanRxToken(self.eth, vid), VLanTxToken(self.eth, vid)))
        } else {
            None
        }
    }
    fn transmit(
        &mut self,
        _timestamp: smoltcp::time::Instant,
    ) -> Option<Self::TxToken<'a>> {
        let vid = self.vid.cfg().vid;
        if self.eth.can_send() {
            Some(VLanTxToken(self.eth, vid))
        } else {
            None
        }
    }
    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        crate::ethernet_capabilities(self.eth)
    }
}

impl DeviceExt for VLanEthernet<'_> {
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
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0.vlan_recv(self.1, f)
    }
}

pub struct VLanTxToken<'a>(&'a eth::Ethernet, u16);
impl<'a> smoltcp::phy::TxToken for VLanTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0
            .vlan_try_send(len, self.1, f)
            .expect("TX token existed without descriptor available")
    }
}

////////////////////////////////////////////////////////////////////////////////

pub type ServerImpl<'a, B> = GenServerImpl<'a, B, VLanEthernet<'a>>;

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
        |vid| VLanEthernet { eth, vid },
    )
}
