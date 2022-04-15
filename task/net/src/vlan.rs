// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32h7_eth::Ethernet;

pub struct VLanRxToken<'a>(&'a Ethernet, u16);

impl<'a> smoltcp::phy::RxToken for VLanRxToken<'a> {
    fn consume<R, F>(
        self,
        _timestamp: smoltcp::time::Instant,
        f: F,
    ) -> smoltcp::Result<R>
    where
        F: FnOnce(&mut [u8]) -> smoltcp::Result<R>,
    {
        self.0
            .vlan_try_recv(f, self.1)
            .expect("we checked RX availability earlier")
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VLanTxToken<'a>(&'a Ethernet, u16);

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
