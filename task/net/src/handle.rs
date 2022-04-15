// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32h7_eth as eth;
use eth::Ethernet;

pub struct EthernetHandle<'a> {
    pub eth: &'a Ethernet,

    #[cfg(feature = "vlan")]
    pub vid: u16,
}

#[cfg(feature = "vlan")]
impl<'a, 'b> smoltcp::phy::Device<'a> for EthernetHandle<'b> {
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

#[cfg(not(feature = "vlan"))]
impl<'a, 'b> smoltcp::phy::Device<'a> for EthernetHandle<'b> {
    type RxToken = eth::OurRxToken<'a>;
    type TxToken = eth::OurTxToken<'a>;

    fn receive(&'a mut self) -> Option<(Self::RxToken, Self::TxToken)> {
        if self.eth.can_recv() && self.eth.can_send() {
            Some((eth::OurRxToken(self.eth), eth::OurTxToken(self.eth)))
        } else {
            None
        }
    }
    fn transmit(&'a mut self) -> Option<Self::TxToken> {
        if self.eth.can_send() {
            Some(eth::OurTxToken(self.eth))
        } else {
            None
        }
    }
    fn capabilities(&self) -> smoltcp::phy::DeviceCapabilities {
        self.eth.capabilities()
    }
}
