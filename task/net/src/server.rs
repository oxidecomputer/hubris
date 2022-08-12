// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::idl;
use drv_stm32h7_eth as eth;
use idol_runtime::RequestError;
use smoltcp::wire::EthernetAddress;
use task_net_api::{
    KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError, RecvError,
    SendError, SocketName, UdpMetadata,
};

/// Abstraction trait to reduce code duplication between VLAN and non-VLAN
/// server implementations.
pub trait NetServer {
    fn net_recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        large_payload_behavior: LargePayloadBehavior,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<RecvError>>;

    fn net_send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<SendError>>;

    fn eth_bsp(&mut self) -> (&eth::Ethernet, &mut crate::bsp::Bsp);

    /// Returns the MAC address for port 0
    fn base_mac_address(&self) -> &EthernetAddress;
}

/// Implementation of the Net Idol interface.
impl<T: NetServer> idl::InOrderNetImpl for T {
    fn recv_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        large_payload_behavior: LargePayloadBehavior,
        payload: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<RecvError>> {
        self.net_recv_packet(msg, socket, large_payload_behavior, payload)
    }

    fn send_packet(
        &mut self,
        msg: &userlib::RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<SendError>> {
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
    ) -> Result<u16, RequestError<PhyError>> {
        use vsc7448_pac::types::PhyRegisterAddress;
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        let (eth, bsp) = self.eth_bsp();
        let out = bsp.phy_read(port, addr, eth)?;
        Ok(out)
    }

    fn write_phy_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        page: u16,
        reg: u8,
        value: u16,
    ) -> Result<(), RequestError<PhyError>> {
        use vsc7448_pac::types::PhyRegisterAddress;
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        let (eth, bsp) = self.eth_bsp();
        bsp.phy_write(port, addr, value, eth)?;
        Ok(())
    }

    fn get_mac_address(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<MacAddress, RequestError<core::convert::Infallible>> {
        let out = self.base_mac_address();
        Ok(MacAddress(out.0))
    }

    ////////////////////////////////////////////////////////////////////////////
    // Stubs for KSZ8463 functions when it's not present
    #[cfg(not(feature = "ksz8463"))]
    fn read_ksz8463_mac_count(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    #[cfg(not(feature = "ksz8463"))]
    fn read_ksz8463_mac(
        &mut self,
        _msg: &userlib::RecvMessage,
        _i: u16,
    ) -> Result<KszMacTableEntry, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    #[cfg(not(feature = "ksz8463"))]
    fn read_ksz8463_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        _i: u16,
    ) -> Result<u16, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    ////////////////////////////////////////////////////////////////////////////
    // Main KSZ8463 functions
    #[cfg(feature = "ksz8463")]
    fn read_ksz8463_mac_count(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<KszError>> {
        let (_eth, bsp) = self.eth_bsp();
        let ksz8463 = bsp.ksz8463();
        let out = ksz8463
            .read_dynamic_mac_table(0)
            .map_err(KszError::from)?
            .map(|mac| mac.count as usize)
            .unwrap_or(0);
        Ok(out)
    }

    #[cfg(feature = "ksz8463")]
    fn read_ksz8463_mac(
        &mut self,
        _msg: &userlib::RecvMessage,
        i: u16,
    ) -> Result<KszMacTableEntry, RequestError<KszError>> {
        if i >= 1024 {
            return Err(KszError::BadMacIndex).map_err(RequestError::from);
        }
        let (_eth, bsp) = self.eth_bsp();
        let ksz8463 = bsp.ksz8463();
        let out = ksz8463
            .read_dynamic_mac_table(i)
            .map_err(KszError::from)?
            .map(KszMacTableEntry::from)
            .unwrap_or(KszMacTableEntry {
                mac: [0; 6],
                port: 0xFFFF,
            });
        Ok(out)
    }

    #[cfg(feature = "ksz8463")]
    fn read_ksz8463_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        i: u16,
    ) -> Result<u16, RequestError<KszError>> {
        use userlib::FromPrimitive;

        let (_eth, bsp) = self.eth_bsp();
        let ksz8463 = bsp.ksz8463();
        let reg =
            ksz8463::Register::from_u16(i).ok_or(KszError::BadRegister)?;
        let out = ksz8463.read(reg).map_err(KszError::from)?;
        Ok(out)
    }

    ////////////////////////////////////////////////////////////////////////////
    // Management network functions, if it's not present
    #[cfg(not(feature = "mgmt"))]
    fn management_link_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ManagementLinkStatus, RequestError<MgmtError>> {
        Err(MgmtError::NotAvailable.into())
    }

    #[cfg(not(feature = "mgmt"))]
    fn management_counters(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ManagementCounters, RequestError<MgmtError>> {
        Err(MgmtError::NotAvailable.into())
    }

    #[cfg(feature = "mgmt")]
    fn management_link_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ManagementLinkStatus, RequestError<MgmtError>> {
        let (eth, bsp) = self.eth_bsp();
        let out = bsp.management_link_status(eth).map_err(MgmtError::from)?;
        Ok(out)
    }

    #[cfg(feature = "mgmt")]
    fn management_counters(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ManagementCounters, RequestError<MgmtError>> {
        let (eth, bsp) = self.eth_bsp();
        let out = bsp.management_counters(eth).map_err(MgmtError::from)?;
        Ok(out)
    }
}
