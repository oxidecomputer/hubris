// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp_support;
use crate::generated::{self, SOCKET_COUNT};
use crate::{idl, ETH_IRQ, NEIGHBORS, WAKE_IRQ_BIT};

use drv_stm32h7_eth as eth;
use idol_runtime::RequestError;
use task_net_api::{
    KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError, RecvError,
    SendError, SocketName, UdpMetadata,
};

use core::iter::zip;
use heapless::Vec;
use smoltcp::iface::{Interface, Neighbor, SocketHandle, SocketStorage};
use smoltcp::socket::UdpSocket;
use smoltcp::wire::{
    EthernetAddress, IpAddress, IpCidr, Ipv6Address, Ipv6Cidr,
};
use userlib::{sys_post, sys_refresh_task_id, UnwrapLite};

/// Abstraction trait to reduce code duplication between VLAN and non-VLAN
/// server implementations.
pub trait NetServer {
    type Bsp: bsp_support::Bsp;

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
}

/// Implementation of the Net Idol interface.
impl<B, E, const N: usize> idl::InOrderNetImpl for GenServerImpl<'_, B, E, N>
where
    Self: NetServer,
    B: bsp_support::Bsp,
    E: DeviceExt,
{
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

pub trait DeviceExt: for<'d> smoltcp::phy::Device<'d> {
    fn read_and_clear_activity_flag(&self) -> bool;
}

/// State for the running network server
pub struct GenServerImpl<'a, B, E, const N: usize>
where
    E: DeviceExt,
{
    eth: &'a eth::Ethernet,

    vlan_state: [VLanState<E>; N],
    client_waiting_to_send: [bool; SOCKET_COUNT],
    bsp: B,

    mac: EthernetAddress,
}

struct VLanState<E>
where
    E: DeviceExt,
{
    socket_handles: [SocketHandle; SOCKET_COUNT],
    iface: Interface<'static, E>,
}

impl<'a, B, E, const N: usize> GenServerImpl<'a, B, E, N>
where
    B: bsp_support::Bsp,
    E: DeviceExt,
{
    /// Size of buffer that must be allocated to use `dispatch`.
    pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

    /// Builds a new `ServerImpl`, using the provided storage space.
    pub(crate) fn new(
        eth: &'a eth::Ethernet,
        mut ipv6_addr: Ipv6Address,
        mut mac: EthernetAddress,
        bsp: B,
        storage: &'static mut [Storage; N],
        sockets: generated::Sockets<'static, N>,
        mut mkdevice: impl FnMut(usize) -> E,
    ) -> Self {
        // Local storage; this will end up owned by the returned ServerImpl.
        let mut vlan_state: Vec<VLanState<E>, N> = Vec::new();

        let start_mac = mac;
        // Each of these is replicated once per VID. Loop over them in lockstep.
        for (i, (sockets, storage)) in zip(sockets.0, storage).enumerate() {
            // Make some types explicit to try and make this clearer.
            let sockets: [UdpSocket<'_>; SOCKET_COUNT] = sockets;

            let neighbor_cache =
                smoltcp::iface::NeighborCache::new(&mut storage.neighbors[..]);

            let builder = smoltcp::iface::InterfaceBuilder::new(
                mkdevice(i),
                &mut storage.sockets[..],
            );

            storage.net = Ipv6Cidr::new(ipv6_addr, 64).into();
            let mut iface = builder
                .hardware_addr(mac.into())
                .neighbor_cache(neighbor_cache)
                .ip_addrs(core::slice::from_mut(&mut storage.net))
                .finalize();

            // Associate sockets with this interface.
            let socket_handles = sockets.map(|s| iface.add_socket(s));
            // Bind sockets to their ports.
            for (&h, port) in zip(&socket_handles, generated::SOCKET_PORTS) {
                iface
                    .get_socket::<UdpSocket<'_>>(h)
                    .bind((ipv6_addr, port))
                    .unwrap_lite();
            }

            vlan_state
                .push(VLanState {
                    socket_handles,
                    iface,
                })
                .unwrap_lite();

            // Increment the MAC and IP addresses so that each VLAN has
            // a unique address.
            ipv6_addr.0[15] += 1;
            mac.0[5] += 1;
        }

        Self {
            eth,
            client_waiting_to_send: [false; SOCKET_COUNT],
            vlan_state: vlan_state.into_array().unwrap_lite(),
            bsp,
            mac: start_mac,
        }
    }

    pub(crate) fn poll(&mut self, t: u64) -> smoltcp::Result<crate::Activity> {
        let t = smoltcp::time::Instant::from_millis(t as i64);
        // Do not be tempted to use `Iterator::any` here, it short circuits and
        // we really do want to poll all of them.
        let mut ip = false;
        let mut mac_rx = false;
        for vlan in &mut self.vlan_state {
            ip |= vlan.iface.poll(t)?;
            // Test and clear our receive activity flag.
            mac_rx |= vlan.iface.device().read_and_clear_activity_flag();
        }

        Ok(crate::Activity { ip, mac_rx })
    }

    /// Iterate over sockets, waking any that can do work.  A task can do work
    /// if all of the (internal) VLAN sockets can receive a packet, since
    /// we don't know which VLAN it will write to.
    pub fn wake_sockets(&mut self) {
        for i in 0..SOCKET_COUNT {
            if (0..N).any(|v| {
                let want_to_send = self.client_waiting_to_send[i];
                let socket = self.get_socket_mut(i, v).unwrap();
                socket.can_recv() || (want_to_send && socket.can_send())
            }) {
                let (task_id, notification) = generated::SOCKET_OWNERS[i];
                let task_id = sys_refresh_task_id(task_id);
                sys_post(task_id, notification);
            }
        }
    }

    pub fn wake(&self) {
        self.bsp.wake(self.eth)
    }

    fn get_handle(
        &self,
        index: usize,
        vlan_index: usize,
    ) -> Option<SocketHandle> {
        self.vlan_state
            .get(vlan_index)?
            .socket_handles
            .get(index)
            .cloned()
    }

    /// Gets the socket `index`. If `index` is out of range, returns
    /// `None`. Panics if `vlan_index` is out of range, which should
    /// never happen (because messages with invalid VIDs are dropped in
    /// RxRing).
    ///
    /// Sockets are currently assumed to be UDP.
    pub(crate) fn get_socket_mut(
        &mut self,
        index: usize,
        vlan_index: usize,
    ) -> Option<&mut UdpSocket<'static>> {
        Some(
            self.vlan_state[vlan_index]
                .iface
                .get_socket::<UdpSocket<'_>>(
                    self.get_handle(index, vlan_index)?,
                ),
        )
    }

    fn eth_bsp(&mut self) -> (&eth::Ethernet, &mut B) {
        (self.eth, &mut self.bsp)
    }

    fn base_mac_address(&self) -> &EthernetAddress {
        &self.mac
    }

    pub(crate) fn set_client_waiting_to_send(&mut self, i: usize, f: bool) {
        self.client_waiting_to_send[i] = f;
    }
}

impl<B, E, const N: usize> idol_runtime::NotificationHandler
    for GenServerImpl<'_, B, E, N>
where
    E: DeviceExt,
{
    fn current_notification_mask(&self) -> u32 {
        // We're always listening for our interrupt or the wake (timer) irq
        ETH_IRQ | 1 << WAKE_IRQ_BIT
    }

    fn handle_notification(&mut self, bits: u32) {
        // Interrupt dispatch.
        if bits & ETH_IRQ != 0 {
            self.eth.on_interrupt();
            userlib::sys_irq_control(ETH_IRQ, true);
        }
        // The wake IRQ is handled in the main `net` loop
    }
}

type NeighborStorage = Option<(IpAddress, Neighbor)>;

pub struct Storage {
    neighbors: [NeighborStorage; NEIGHBORS],
    sockets: [SocketStorage<'static>; SOCKET_COUNT],
    net: IpCidr,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            neighbors: Default::default(),
            sockets: Default::default(),
            net: Ipv6Cidr::default().into(),
        }
    }
}
