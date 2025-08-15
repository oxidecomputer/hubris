// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp_support;
use crate::generated::{self, SOCKET_COUNT};
use crate::notifications;
use crate::{idl, link_local_iface_addr, MacAddressBlock};

use drv_stm32h7_eth as eth;
use enum_map::Enum;
use idol_runtime::{ClientError, RequestError};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_net_api::{
    KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
    ManagementCounters, ManagementLinkStatus, MgmtError, PhyError, RecvError,
    SendError, SocketName, TrustError, UdpMetadata, VLanId,
};

#[allow(dead_code)]
#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    SetTrustUntil {
        #[count(children)]
        vid: VLanId,
        until: u64,
    },
    SetDistrust {
        #[count(children)]
        vid: VLanId,
    },
    TrustExpired {
        #[count(children)]
        vid: VLanId,
    },
    SkipSendUntrustedPacket {
        #[count(children)]
        vid: VLanId,
    },
    SkipReceiveUntrustedPacket {
        #[count(children)]
        vid: VLanId,
    },
}
counted_ringbuf!(Trace, 16, Trace::None);

use core::iter::zip;
use heapless::Vec;
use smoltcp::iface::{Interface, SocketHandle, SocketStorage};
use smoltcp::socket::udp;
use smoltcp::wire::{EthernetAddress, Ipv6Cidr};
use userlib::{sys_get_timer, sys_post, sys_refresh_task_id, UnwrapLite};
use zerocopy::byteorder::U16;

/// Implementation of the Net Idol interface.
impl<B, E> idl::InOrderNetImpl for GenServerImpl<'_, B, E>
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
            return Err(RequestError::from(KszError::BadMacIndex));
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
        let out = bsp.management_link_status(eth)?;
        Ok(out)
    }

    #[cfg(feature = "mgmt")]
    fn management_counters(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ManagementCounters, RequestError<MgmtError>> {
        let (eth, bsp) = self.eth_bsp();
        let out = bsp.management_counters(eth)?;
        Ok(out)
    }

    #[cfg(feature = "vlan")]
    fn trust_vlan(
        &mut self,
        _msg: &userlib::RecvMessage,
        vid: VLanId,
        trust_until: u64,
    ) -> Result<(), RequestError<TrustError>> {
        ringbuf_entry!(Trace::SetTrustUntil {
            vid,
            until: trust_until
        });
        self.set_vlan_trust(vid, VLanTrust::TrustUntil(trust_until))
            .map_err(RequestError::from)
    }

    #[cfg(feature = "vlan")]
    fn distrust_vlan(
        &mut self,
        _msg: &userlib::RecvMessage,
        vid: VLanId,
    ) -> Result<(), RequestError<TrustError>> {
        ringbuf_entry!(Trace::SetDistrust { vid });
        self.set_vlan_trust(vid, VLanTrust::Distrust)
            .map_err(RequestError::from)
    }

    #[cfg(not(feature = "vlan"))]
    fn trust_vlan(
        &mut self,
        _msg: &userlib::RecvMessage,
        _vid: VLanId,
        _trust_until: u64,
    ) -> Result<(), RequestError<TrustError>> {
        Err(TrustError::NoSuchVLAN.into())
    }

    #[cfg(not(feature = "vlan"))]
    fn distrust_vlan(
        &mut self,
        _msg: &userlib::RecvMessage,
        _vid: VLanId,
    ) -> Result<(), RequestError<TrustError>> {
        Err(TrustError::NoSuchVLAN.into())
    }
}

pub trait DeviceExt: smoltcp::phy::Device {
    fn make_meta(
        &self,
        port: u16,
        size: usize,
        addr: task_net_api::Address,
    ) -> UdpMetadata;
}

/// State for the running network server
pub struct GenServerImpl<'a, B, E>
where
    E: DeviceExt,
{
    eth: &'a eth::Ethernet,

    vlan_state: enum_map::EnumMap<VLanId, VLanState<E>>,
    client_waiting_to_send: [bool; SOCKET_COUNT],
    bsp: B,

    mac: EthernetAddress,
    spare_macs: MacAddressBlock,
}

/// Configuration
#[cfg_attr(not(feature = "vlan"), allow(dead_code))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VLanTrust {
    AlwaysTrust,
    TrustUntil(u64),
    Distrust,
}

struct VLanState<E>
where
    E: DeviceExt,
{
    vid: VLanId, // used for logging

    socket_handles: [SocketHandle; SOCKET_COUNT],
    socket_set: smoltcp::iface::SocketSet<'static>,
    iface: &'static mut Interface,
    device: E,
    trust: VLanTrust,

    /// Used to detect stuck queues (due to smoltcp#594)
    queue_watchdog: [QueueWatchdog; SOCKET_COUNT],
}

impl<E: DeviceExt> VLanState<E> {
    fn check_trust(&mut self, now: u64) -> bool {
        match self.trust {
            VLanTrust::AlwaysTrust => true,
            VLanTrust::Distrust => false,
            VLanTrust::TrustUntil(t) => {
                if now >= t {
                    ringbuf_entry!(Trace::TrustExpired { vid: self.vid });
                    self.trust = VLanTrust::Distrust;
                    false
                } else {
                    true
                }
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum QueueWatchdog {
    /// Data is flowing through the queue
    Nominal,

    /// We have seen a `QueueFull` error, and no packets have successfully
    /// entered the queue since then
    QueueFullAt(u64),

    /// We have seen two `QueueFull` errors separated by some timeout without
    /// any packets successfully entering the queue in between.  This indicates
    /// that the queue is likely stuck.
    QueueFullTimeout,
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
    ) -> Option<&mut udp::Socket<'static>> {
        Some(
            self.socket_set
                .get_mut::<udp::Socket<'_>>(self.get_handle(index)?),
        )
    }

    pub(crate) fn check_socket_watchdog(&mut self) -> bool {
        let mut changed = false;
        for socket_index in 0..SOCKET_COUNT {
            if self.queue_watchdog[socket_index]
                == QueueWatchdog::QueueFullTimeout
            {
                // Reset the queue by closing + reopening it.  This will lose
                // packets in the RX queue as well; they're collateral damage
                // because `smoltcp` doesn't expose a way to flush just the TX
                // side.
                let s = self.get_socket_mut(socket_index).unwrap_lite();
                let e = s.endpoint();
                s.close();
                s.bind(e).unwrap_lite();
                changed = true;

                // Reset the watchdog, so it doesn't fire right away
                self.queue_watchdog[socket_index] = QueueWatchdog::Nominal;
            }
        }
        changed
    }
}

impl<'a, B, E> GenServerImpl<'a, B, E>
where
    B: bsp_support::Bsp,
    E: DeviceExt,
{
    /// Builds a new `ServerImpl`, using the provided storage space.
    pub(crate) fn new(
        eth: &'a eth::Ethernet,
        mac_address_block: MacAddressBlock,
        bsp: B,
        storage: &'static mut [Storage; VLanId::LENGTH],
        sockets: generated::Sockets<'static, { VLanId::LENGTH }>,
        mut mkdevice: impl FnMut(VLanId) -> E,
    ) -> Self {
        // Local storage; this will end up owned by the returned ServerImpl.
        let mut vlan_state: Vec<VLanState<E>, { VLanId::LENGTH }> = Vec::new();

        // Did you bring enough MAC addresses for everyone?
        assert!(
            mac_address_block.count.get() as usize >= generated::PORT_COUNT
        );

        let mut port_to_mac: [[u8; 6]; generated::PORT_COUNT] =
            Default::default();
        let mut mac: [u8; 6] = mac_address_block.base_mac;
        for p in port_to_mac.iter_mut() {
            *p = mac;

            // Increment the MAC and IP addresses based on the stride in the
            // configuration block, so that each port has a unique address.
            //
            // We only want to increment the lower 3 octets, leaving the OUI
            // (top 3 octets) the same.
            //
            // It's a *little* awkward: We need a `[u8; 4]` to call
            // `u32::from_be_bytes`, but only care about the lower 24 bits.
            // To work around this, we include one octet of the OUI when
            // converting into a `u32`, then mask the resulting value with
            // `0xFFFFFF` afterwards.
            let next_mac = (u32::from_be_bytes(mac[2..].try_into().unwrap())
                & 0xFFFFFF)
                + mac_address_block.stride as u32;

            // Per https://github.com/oxidecomputer/oana/#mac-addresses, we
            // reserve `F0:00:00` and above for software stuff if we're
            // using the Oxide OUI
            const OXIDE_OUI: [u8; 3] = [0xa8, 0x40, 0x25];
            if mac[..3] == OXIDE_OUI && next_mac > 0xEFFFFF {
                panic!("MAC overflow: {:?}", mac);
            }

            // Copy back into the (mutable) current MAC address
            mac[3..].copy_from_slice(&next_mac.to_be_bytes()[1..]);
        }

        // Each of these is replicated once per VID. Loop over them in lockstep.
        for (i, (sockets, storage)) in zip(sockets.0, storage).enumerate() {
            #[cfg(feature = "vlan")]
            let (vlan_id, mac, trust) = {
                let vlan_id = VLanId::from_usize(i);
                (
                    vlan_id,
                    port_to_mac[match vlan_id.cfg().port {
                        task_net_api::SpPort::One => 0,
                        task_net_api::SpPort::Two => 1,
                    }],
                    if vlan_id.cfg().always_trusted {
                        VLanTrust::AlwaysTrust
                    } else {
                        VLanTrust::Distrust
                    },
                )
            };

            #[cfg(not(feature = "vlan"))]
            let (vlan_id, mac, trust) = {
                let _ = i; // avoid warnings about unused variable
                (VLanId::None, port_to_mac[0], VLanTrust::AlwaysTrust)
            };

            let mac_addr = EthernetAddress::from_bytes(&mac);
            let ipv6_addr = link_local_iface_addr(mac_addr);

            // Make some types explicit to try and make this clearer.
            let sockets: [udp::Socket<'_>; SOCKET_COUNT] = sockets;

            let mut config = smoltcp::iface::Config::new();
            config.hardware_addr = Some(mac_addr.into());
            let mut device = mkdevice(vlan_id);
            let iface =
                storage.iface.write(Interface::new(config, &mut device));
            iface.update_ip_addrs(|ip_addrs| {
                ip_addrs.push(Ipv6Cidr::new(ipv6_addr, 64).into()).unwrap()
            });

            // Associate sockets with this interface.
            let mut socket_set =
                smoltcp::iface::SocketSet::new(storage.sockets.as_mut_slice());
            let socket_handles = sockets.map(|s| socket_set.add(s));
            // Bind sockets to their ports.
            for (&h, port) in zip(&socket_handles, generated::SOCKET_PORTS) {
                socket_set
                    .get_mut::<udp::Socket<'_>>(h)
                    .bind((ipv6_addr, port))
                    .unwrap_lite();
            }

            vlan_state
                .push(VLanState {
                    vid: vlan_id,
                    socket_handles,
                    iface,
                    device,
                    trust,
                    socket_set,
                    queue_watchdog: [QueueWatchdog::Nominal; SOCKET_COUNT],
                })
                .unwrap_lite();
        }

        Self {
            eth,
            // The 'true' here is load-bearing: it ensures that sockets receive
            // a notification on stack restart.
            client_waiting_to_send: [true; SOCKET_COUNT],
            vlan_state: enum_map::EnumMap::from_array(
                vlan_state.into_array().unwrap_lite(),
            ),
            bsp,
            mac: EthernetAddress::from_bytes(&mac_address_block.base_mac),
            spare_macs: MacAddressBlock {
                base_mac: mac,
                count: U16::new(
                    mac_address_block.count.get() - VLanId::LENGTH as u16,
                ),
                stride: mac_address_block.stride,
            },
        }
    }

    pub(crate) fn poll(&mut self, t: u64) -> crate::Activity {
        let instant = smoltcp::time::Instant::from_millis(t as i64);
        // Do not be tempted to use `Iterator::any` here, it short circuits and
        // we really do want to poll all of them.
        let mut ip = false;
        for vlan in self.vlan_state.values_mut() {
            ip |= vlan.iface.poll(
                instant,
                &mut vlan.device,
                &mut vlan.socket_set,
            );
            // Test and clear our receive activity flag.
            ip |= vlan.check_socket_watchdog();
        }

        crate::Activity { ip }
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
                .values_mut()
                .any(|v| v.get_socket_mut(i).unwrap().can_recv());
            // send wake only happens if the wait flag is set.
            let send_wake = self.client_waiting_to_send[i]
                && self
                    .vlan_state
                    .values_mut()
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
            return Err(ClientError::AccessViolation.fail());
        }
        let now = sys_get_timer().now;

        // Iterate over all of the per-VLAN sockets, returning the first
        // available packet with a bonus `vid` tag attached in the metadata.
        for vlan in self.vlan_state.values_mut() {
            // Decide whether to pass this packet to the socket, depending on
            // whether we trust the VLAN or not.  Sockets can be configured to
            // accept even untrusted packets (e.g. control_plane_agent needs to
            // receive an unlock message).
            let trust = vlan.check_trust(now)
                | generated::SOCKET_ALLOW_UNTRUSTED[socket_index];
            let vid = vlan.vid; // for logging

            let socket = vlan
                .get_socket_mut(socket_index)
                .ok_or(RequestError::Fail(ClientError::BadMessageContents))?;
            #[allow(clippy::while_let_loop)]
            loop {
                match socket.recv() {
                    Ok((body, endp)) => {
                        // Drop packets from untrusted VLANs after receiving
                        // them (to avoid clogging the queue)
                        if !trust {
                            ringbuf_entry!(Trace::SkipReceiveUntrustedPacket {
                                vid
                            });
                            continue;
                        }

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

                        return Ok(vlan.device.make_meta(
                            endp.port,
                            body_len,
                            endp.addr.try_into().map_err(|_| ()).unwrap(),
                        ));
                    }
                    Err(udp::RecvError::Exhausted) => {
                        // Move on to next vid
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
            return Err(ClientError::AccessViolation.fail());
        }

        let now = userlib::sys_get_timer().now;

        #[cfg(feature = "vlan")]
        let vlan = {
            let vlan = &mut self.vlan_state[metadata.vid];
            // Refuse to send messages directed to an untrusted VLAN, silently
            // dropping them.
            let trust = vlan.check_trust(now)
                | generated::SOCKET_ALLOW_UNTRUSTED[socket_index];
            if !trust {
                #[cfg(feature = "vlan")]
                ringbuf_entry!(Trace::SkipSendUntrustedPacket {
                    vid: metadata.vid
                });
                return Ok(());
            }
            vlan
        };

        #[cfg(not(feature = "vlan"))]
        let vlan = &mut self.vlan_state[VLanId::None];

        let socket = vlan
            .get_socket_mut(socket_index)
            .ok_or(RequestError::Fail(ClientError::BadMessageContents))?;
        match socket.send(payload.len(), metadata.into()) {
            Ok(buf) => {
                payload
                    .read_range(0..payload.len(), buf)
                    .map_err(|_| RequestError::went_away())?;
                self.client_waiting_to_send[socket_index] = false;
                vlan.queue_watchdog[socket_index] = QueueWatchdog::Nominal;
                Ok(())
            }
            Err(udp::SendError::BufferFull) => {
                const SOCKET_QUEUE_FULL_TIMEOUT_MS: u64 = 500;

                // Record a new QueueFull error if the socket had been working
                // until now, or roll over into QueueFullTimeout if we've
                // exceeded our timeout delay.
                match vlan.queue_watchdog[socket_index] {
                    QueueWatchdog::Nominal => {
                        vlan.queue_watchdog[socket_index] =
                            QueueWatchdog::QueueFullAt(now)
                    }
                    QueueWatchdog::QueueFullAt(t) => {
                        if now >= t + SOCKET_QUEUE_FULL_TIMEOUT_MS {
                            vlan.queue_watchdog[socket_index] =
                                QueueWatchdog::QueueFullTimeout
                        }
                    }
                    QueueWatchdog::QueueFullTimeout => (),
                }
                self.client_waiting_to_send[socket_index] = true;
                Err(SendError::QueueFull.into())
            }
            Err(udp::SendError::Unaddressable) => {
                // smoltcp's "Unaddressable" case may not be what you'd expect
                // from the name. It indicates that the address and/or port
                // provided by the caller are _statically invalid,_ such as port
                // 0 or address `[::]`. It does _not_ mean unreachable.
                //
                // This means that this error indicates a precondition violation
                // by the client, which in turn means: kill kill kill
                Err(ClientError::BadMessageContents.fail())
            }
        }
    }

    #[cfg(feature = "vlan")]
    fn set_vlan_trust(
        &mut self,
        vid: VLanId,
        t: VLanTrust,
    ) -> Result<(), TrustError> {
        if vid.cfg().always_trusted {
            return Err(TrustError::AlwaysTrusted);
        }
        self.vlan_state[vid].trust = t;
        Ok(())
    }
}

impl<B, E> idol_runtime::NotificationHandler for GenServerImpl<'_, B, E>
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

pub struct Storage {
    sockets: [SocketStorage<'static>; SOCKET_COUNT],
    iface: core::mem::MaybeUninit<Interface>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            sockets: Default::default(),
            iface: core::mem::MaybeUninit::uninit(),
        }
    }
}
