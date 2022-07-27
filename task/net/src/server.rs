// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_stm32h7_eth as eth;

use crate::idl;
use idol_runtime::RequestError;
use task_net_api::{NetError, SocketName, UdpMetadata};

/// Abstraction trait to reduce code duplication between VLAN and non-VLAN
/// server implementations.
pub trait NetServer {
    fn net_recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<NetError>>;

    fn net_send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<NetError>>;

    fn eth_bsp(&mut self) -> (&eth::Ethernet, &mut crate::bsp::Bsp);
}

/// Implementation of the Net Idol interface.
impl<T: NetServer> idl::InOrderNetImpl for T {
    fn recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<NetError>> {
        self.net_recv_packet(msg, socket, payload)
    }

    fn send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<NetError>> {
        self.net_send_packet(msg, socket, metadata, payload)
    }

    fn smi_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
    ) -> Result<u16, idol_runtime::RequestError<core::convert::Infallible>>
    {
        // TODO: this should not be open to all callers!
        Ok(self.eth_bsp().0.smi_read(phy, register))
    }

    fn smi_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        phy: u8,
        register: u8,
        value: u16,
    ) -> Result<(), idol_runtime::RequestError<core::convert::Infallible>> {
        // TODO: this should not be open to all callers!
        self.eth_bsp().0.smi_write(phy, register, value);
        Ok(())
    }

    fn read_phy_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        page: u16,
        reg: u8,
    ) -> Result<u16, RequestError<NetError>> {
        if port >= 2 {
            return Err(NetError::InvalidPort.into());
        }
        use vsc7448_pac::types::PhyRegisterAddress;
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        let (eth, bsp) = self.eth_bsp();
        let out = bsp
            .phy_fn(port, |phy| phy.read(addr), eth)?
            .map_err(|_| NetError::Other)?;
        Ok(out)
    }

    fn write_phy_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        page: u16,
        reg: u8,
        value: u16,
    ) -> Result<(), RequestError<NetError>> {
        if port > 2 {
            return Err(NetError::InvalidPort.into());
        }
        use vsc7448_pac::types::PhyRegisterAddress;
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        let (eth, bsp) = self.eth_bsp();
        bsp.phy_fn(port, |phy| phy.write(addr, value), eth)?
            .map_err(|_| NetError::Other)?;
        Ok(())
    }
}
