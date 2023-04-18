// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp_support;
use crate::generated::{self, SOCKET_COUNT};
use crate::notifications;
use crate::{idl, link_local_iface_addr, MacAddressBlock, NEIGHBORS};

#[cfg(feature = "vlan")]
use crate::generated::VLAN_RANGE;

use drv_stm32h7_eth as eth;
use idol_runtime::{ClientError, RequestError};
use task_net_api::{
    KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError, RecvError,
    SendError, SocketName, UdpMetadata,
};

use core::iter::zip;
use heapless::Vec;
use smoltcp::iface::{Interface, Neighbor, SocketHandle, SocketStorage};
use smoltcp::socket::UdpSocket;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv6Cidr};
use userlib::{sys_post, sys_refresh_task_id, UnwrapLite};
use zerocopy::byteorder::U16;

/// Implementation of the Net Idol interface.
impl<B, E, const N: usize> idl::InOrderNetImpl for GenServerImpl<'_, B, E, N>
where
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

    fn get_spare_mac_addresses(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<MacAddressBlock, RequestError<core::convert::Infallible>> {
        Ok(self.spare_macs)
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
            .unwrap_lite()
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
            .unwrap_lite()
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
        let out = ksz8463.read(reg).unwrap_lite();
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

    fn make_meta(
        &self,
        port: u16,
        size: usize,
        addr: task_net_api::Address,
    ) -> UdpMetadata;
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
    spare_macs: MacAddressBlock,
}

struct VLanState<E>
where
    E: DeviceExt,
{
    socket_handles: [SocketHandle; SOCKET_COUNT],
    iface: Interface<'static, E>,

    /// If calls to `net_send_packet` consistently return `QueueFull`, this is
    /// the time at which the _first_ error was returned.  It is cleared when a
    /// call to `net_send_packet` succeeds.
    ///
    /// Used as a watchdog to detect stuck queues
    first_queue_full: [Option<u64>; SOCKET_COUNT],
}

impl<E: DeviceExt> VLanState<E> {
    fn get_handle(&self, index: usize) -> Option<SocketHandle> {
        self.socket_handles.get(index).cloned()
    }

    /// Gets the socket `index`. If `index` is out of range, returns
    /// `None`.
    ///
    /// Sockets are currently assumed to be UDP.
    pub(crate) fn get_socket_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut UdpSocket<'static>> {
        Some(
            self.iface
                .get_socket::<UdpSocket<'_>>(self.get_handle(index)?),
        )
    }

    pub(crate) fn check_socket_watchdog(&mut self, t: u64) -> bool {
        const SOCKET_QUEUE_FULL_TIMEOUT_MS: u64 = 500;
        let mut changed = false;
        for socket_index in 0..SOCKET_COUNT {
            if let Some(prev_time) = self.first_queue_full[socket_index] {
                if t >= prev_time + SOCKET_QUEUE_FULL_TIMEOUT_MS {
                    let s = self.get_socket_mut(socket_index).unwrap_lite();
                    if !s.can_send() {
                        // Reset the queue by closing + reopening it.  This
                        // will lose packets in the RX queue as well;
                        // they're collateral damage because `smoltcp`
                        // doesn't expose a way to flush just the TX side.
                        let e = s.endpoint();
                        s.close();
                        s.bind(e).unwrap();
                        changed = true;
                    }
                }
            }
        }
        changed
    }
}

impl<'a, B, E, const N: usize> GenServerImpl<'a, B, E, N>
where
    B: bsp_support::Bsp,
    E: DeviceExt,
{
    /// Builds a new `ServerImpl`, using the provided storage space.
    pub(crate) fn new(
        eth: &'a eth::Ethernet,
        mac_address_block: MacAddressBlock,
        bsp: B,
        storage: &'static mut [Storage; N],
        sockets: generated::Sockets<'static, N>,
        mut mkdevice: impl FnMut(usize) -> E,
    ) -> Self {
        // Local storage; this will end up owned by the returned ServerImpl.
        let mut vlan_state: Vec<VLanState<E>, N> = Vec::new();

        // Did you bring enough MAC addresses for everyone?
        assert!(mac_address_block.count.get() as usize >= N);
        let mut mac: [u8; 6] = mac_address_block.base_mac;

        // Each of these is replicated once per VID. Loop over them in lockstep.
        for (i, (sockets, storage)) in zip(sockets.0, storage).enumerate() {
            let mac_addr = EthernetAddress::from_bytes(&mac);
            let ipv6_addr = link_local_iface_addr(mac_addr);

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
                .hardware_addr(mac_addr.into())
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
                    first_queue_full: [None; SOCKET_COUNT],
                })
                .unwrap_lite();

            // Increment the MAC and IP addresses based on the stride in the
            // configuration block, so that each VLAN has a unique address.
            //
            // We only want to increment the lower 3 octets, leaving the OUI
            // (top 3 octets) the same.
            //
            // It's a *little* awkward:
            // We need a `[u8; 4]` to call `u32::from_be_bytes`, but only care
            // about the lower 24 bits. To work around this, we include one
            // octet of the OUI when converting into a `u32`, then mask the
            // resulting value with `0xFFFFFF` afterwards.
            let next_mac = (u32::from_be_bytes(mac[2..].try_into().unwrap())
                & 0xFFFFFF)
                + mac_address_block.stride as u32;

            // Per https://github.com/oxidecomputer/oana/#mac-addresses, we
            // reserve `F0:00:00` and above for software stuff if we're using
            // the Oxide OUI
            const OXIDE_OUI: [u8; 3] = [0xa8, 0x40, 0x25];
            if mac[..3] == OXIDE_OUI && next_mac > 0xEFFFFF {
                panic!("MAC overflow: {:?}", mac);
            }

            // Copy back into the (mutable) current MAC address
            mac[3..].copy_from_slice(&next_mac.to_be_bytes()[1..]);
        }

        Self {
            eth,
            client_waiting_to_send: [false; SOCKET_COUNT],
            vlan_state: vlan_state.into_array().unwrap_lite(),
            bsp,
            mac: EthernetAddress::from_bytes(&mac_address_block.base_mac),
            spare_macs: MacAddressBlock {
                base_mac: mac,
                count: U16::new(mac_address_block.count.get() - N as u16),
                stride: mac_address_block.stride,
            },
        }
    }

    pub(crate) fn poll(&mut self, t: u64) -> smoltcp::Result<crate::Activity> {
        let instant = smoltcp::time::Instant::from_millis(t as i64);
        // Do not be tempted to use `Iterator::any` here, it short circuits and
        // we really do want to poll all of them.
        let mut ip = false;
        let mut mac_rx = false;
        for vlan in &mut self.vlan_state {
            ip |= vlan.iface.poll(instant)?;
            // Test and clear our receive activity flag.
            mac_rx |= vlan.iface.device().read_and_clear_activity_flag();
            // Check the socket watchdog, which may free up packet space
            ip |= vlan.check_socket_watchdog(t);
        }

        Ok(crate::Activity { ip, mac_rx })
    }

    /// Iterate over sockets, waking any that can do work.
    ///
    /// A task can do work if...
    ///
    /// - any of its sockets (on any VLAN) have incoming packets waiting, or
    ///
    /// - it is waiting to send on some socket S, and _all_ of the copies of S
    ///   across all VLANs can accept an outgoing packet. (The "all" is
    ///   important here since we don't keep track of which one it's trying to
    ///   send through.)
    pub fn wake_sockets(&mut self) {
        for i in 0..SOCKET_COUNT {
            // recv wake depends only on the state of the sockets.
            let recv_wake = self
                .vlan_state
                .iter_mut()
                .any(|v| v.get_socket_mut(i).unwrap().can_recv());
            // send wake only happens if the wait flag is set.
            let send_wake = self.client_waiting_to_send[i]
                && self
                    .vlan_state
                    .iter_mut()
                    .all(|v| v.get_socket_mut(i).unwrap().can_send());

            if recv_wake || send_wake {
                let (task_id, notification) = generated::SOCKET_OWNERS[i];
                let task_id = sys_refresh_task_id(task_id);
                sys_post(task_id, notification);
            }
        }
    }

    pub fn wake(&self) {
        self.bsp.wake(self.eth)
    }

    fn eth_bsp(&mut self) -> (&eth::Ethernet, &mut B) {
        (self.eth, &mut self.bsp)
    }

    fn base_mac_address(&self) -> &EthernetAddress {
        &self.mac
    }

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
        for vlan in &mut self.vlan_state {
            let socket = vlan
                .get_socket_mut(socket_index)
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

                        // Release borrow on self/socket
                        let body_len = body.len();

                        return Ok(vlan.iface.device().make_meta(
                            endp.port,
                            body_len,
                            endp.addr.try_into().map_err(|_| ()).unwrap(),
                        ));
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

        #[cfg(feature = "vlan")]
        let vlan_index = {
            // Convert from absolute VID to an index in our VLAN array
            if !VLAN_RANGE.contains(&metadata.vid) {
                return Err(SendError::InvalidVLan.into());
            }
            usize::from(metadata.vid - VLAN_RANGE.start)
        };
        #[cfg(not(feature = "vlan"))]
        let vlan_index = 0;

        let vlan = &mut self.vlan_state[vlan_index];
        let socket = vlan
            .get_socket_mut(socket_index)
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))?;
        match socket.send(payload.len(), metadata.into()) {
            Ok(buf) => {
                payload
                    .read_range(0..payload.len(), buf)
                    .map_err(|_| RequestError::went_away())?;
                self.client_waiting_to_send[socket_index] = false;
                vlan.first_queue_full[socket_index] = None;
                Ok(())
            }
            Err(smoltcp::Error::Exhausted) => {
                // Record the QueueFull error if this is the first time we've
                // seen it since a successful transmission.
                if vlan.first_queue_full[socket_index].is_none() {
                    let now = userlib::sys_get_timer().now;
                    vlan.first_queue_full[socket_index] = Some(now);
                }
                self.client_waiting_to_send[socket_index] = true;
                Err(SendError::QueueFull.into())
            }
            Err(_e) => {
                // uhhhh TODO
                Err(SendError::Other.into())
            }
        }
    }
}

impl<B, E, const N: usize> idol_runtime::NotificationHandler
    for GenServerImpl<'_, B, E, N>
where
    E: DeviceExt,
{
    fn current_notification_mask(&self) -> u32 {
        notifications::ETH_IRQ_MASK | notifications::WAKE_TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        // Interrupt dispatch.
        if bits & notifications::ETH_IRQ_MASK != 0 {
            self.eth.on_interrupt();
            userlib::sys_irq_control(notifications::ETH_IRQ_MASK, true);
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
