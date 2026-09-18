// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `net` task, for a Hubris system running on the host.
//!
//! This serves the same `Net` IPC interface as `task-net`, so its clients
//! (`udpecho`, `udprpc`, `control_plane_agent`, ...) run unchanged, but its
//! sockets are real UDP sockets opened on the host machine instead of a
//! smoltcp stack on an Ethernet MAC. Each socket in `[config.net.sockets]`
//! is bound to its port (plus `port-offset`), so ordinary tools on the host
//! can talk to the simulated system:
//!
//! ```text
//! [tasks.net]
//! name = "task-host-net"
//! notifications = ["wake-timer"]
//!
//! [tasks.net.config]
//! port-offset = 10000   # echo on 7 is reachable at 10007
//! ```
//!
//! Sockets are bound to `::` by default, which on Linux also accepts IPv4:
//! an IPv4 peer appears to clients as an IPv4-mapped IPv6 address, the only
//! address kind the API has. Link-local peers keep working because the scope
//! a peer was last heard on is remembered and used to reply.
//!
//! A host task can only wake up for IPC or its own timer, so the host
//! sockets are polled every `poll-interval` ticks (5 by default), and socket
//! owners are posted their notification whenever a packet is waiting or a
//! previously full send queue may have drained. Because of this, traffic
//! from outside the simulation only makes sense against the wall clock: run
//! with `cargo xtask host-run --realtime`. In virtual time the polling keeps
//! a timer pending forever, so a run also needs `--stop-at` to end.
//!
//! The PHY, SMI, KSZ8463 and management operations have no host equivalent:
//! they report "not available" (or read as zero, where the interface has no
//! error to give). With the `vlan` feature, all traffic is attributed to the
//! first configured VLAN, and VLAN trust is tracked as on hardware.

#[cfg(target_os = "none")]
compile_error!("task-host-net only runs on the host; use task-net on hardware");

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{
    IpAddr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket,
};

use idol_runtime::{
    ClientError, Leased, NotificationHandler, R, RequestError, W,
};
use task_net_api::{
    Address, Ipv6Address, KszError, KszMacTableEntry, LargePayloadBehavior,
    MacAddress, MacAddressBlock, ManagementCounters, ManagementLinkStatus,
    MgmtError, PhyError, RecvError, SendError, SocketName, TrustError,
    UdpMetadata, VLanId,
};
use userlib::{
    Generation, NotificationBits, RecvMessage, TaskId, set_timer_relative,
    sys_post, sys_refresh_task_id,
};

/// A locally administered address, since there is no board to read one
/// from: `02:48:6f:73:74:00` ("Host").
const MAC_ADDRESS: [u8; 6] = [0x02, 0x48, 0x6f, 0x73, 0x74, 0x00];

/// Largest UDP payload the host can deliver.
const MAX_DATAGRAM: usize = 65535;

struct ServerImpl {
    sockets: Vec<Option<UdpSocket>>,
    /// Sockets whose owner was told the queue was full, and should be woken
    /// to retry.
    waiting_to_send: [bool; config::SOCKET_COUNT],
    /// Scope (interface index) each link-local peer was last heard on.
    scopes: HashMap<[u8; 16], u32>,
    /// Last send error reported per socket, to avoid repeating it.
    last_send_error: [Option<ErrorKind>; config::SOCKET_COUNT],
    scratch: Vec<u8>,
    #[cfg(feature = "vlan")]
    trust: enum_map::EnumMap<VLanId, Trust>,
}

#[cfg(feature = "vlan")]
#[derive(Copy, Clone)]
enum Trust {
    Always,
    Until(u64),
    Distrust,
}

impl ServerImpl {
    fn new() -> Self {
        let bind: IpAddr = config::BIND_ADDRESS.parse().unwrap();
        let sockets = (0..config::SOCKET_COUNT)
            .map(|i| open_socket(i, bind))
            .collect();
        Self {
            sockets,
            waiting_to_send: [false; config::SOCKET_COUNT],
            scopes: HashMap::new(),
            last_send_error: [None; config::SOCKET_COUNT],
            scratch: vec![0; MAX_DATAGRAM],
            #[cfg(feature = "vlan")]
            trust: enum_map::EnumMap::from_fn(|vid: VLanId| {
                if vid.cfg().always_trusted {
                    Trust::Always
                } else {
                    Trust::Distrust
                }
            }),
        }
    }

    fn arm_timer(&self) {
        set_timer_relative(
            config::POLL_INTERVAL,
            notifications::WAKE_TIMER_MASK,
        );
    }

    /// Posts each socket's owner if a packet is waiting for it or it may
    /// retry a send.
    fn wake_owners(&mut self) {
        for i in 0..config::SOCKET_COUNT {
            let readable =
                self.sockets[i].as_ref().is_some_and(has_pending_datagram);
            if readable || self.waiting_to_send[i] {
                self.waiting_to_send[i] = false;
                let (index, mask) = config::SOCKET_OWNERS[i];
                let owner = sys_refresh_task_id(TaskId::for_index_and_gen(
                    index.into(),
                    Generation::ZERO,
                ));
                sys_post(owner, mask);
            }
        }
    }

    /// Checks that `msg` came from the socket's owner.
    fn check_owner(
        msg: &RecvMessage,
        socket: usize,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        if usize::from(config::SOCKET_OWNERS[socket].0) == msg.sender.index() {
            Ok(())
        } else {
            Err(ClientError::AccessViolation.fail())
        }
    }

    #[cfg(feature = "vlan")]
    fn trusted(&mut self, vid: VLanId, socket: usize) -> bool {
        if config::SOCKET_ALLOW_UNTRUSTED[socket] {
            return true;
        }
        let now = userlib::sys_get_timer().now;
        match self.trust[vid] {
            Trust::Always => true,
            Trust::Distrust => false,
            Trust::Until(t) if now >= t => {
                self.trust[vid] = Trust::Distrust;
                false
            }
            Trust::Until(_) => true,
        }
    }

    fn metadata(&mut self, from: SocketAddr, size: usize) -> UdpMetadata {
        let (ip, port) = match from {
            SocketAddr::V6(a) => {
                if a.scope_id() != 0 {
                    self.scopes.insert(a.ip().octets(), a.scope_id());
                }
                (a.ip().octets(), a.port())
            }
            SocketAddr::V4(a) => (a.ip().to_ipv6_mapped().octets(), a.port()),
        };
        UdpMetadata {
            addr: Address::Ipv6(Ipv6Address(ip)),
            port,
            size: size as u32,
            #[cfg(feature = "vlan")]
            vid: first_vlan(),
        }
    }

    /// Where a send to `metadata` should go from a socket bound to `local`.
    fn destination(
        &self,
        local: SocketAddr,
        metadata: &UdpMetadata,
    ) -> Option<SocketAddr> {
        let Address::Ipv6(Ipv6Address(octets)) = metadata.addr;
        let ip = Ipv6Addr::from(octets);
        if local.is_ipv4() {
            let v4 = ip.to_ipv4_mapped()?;
            return Some(SocketAddr::V4(SocketAddrV4::new(v4, metadata.port)));
        }
        let scope = self.scopes.get(&octets).copied().unwrap_or(0);
        Some(SocketAddr::V6(SocketAddrV6::new(
            ip,
            metadata.port,
            0,
            scope,
        )))
    }
}

#[cfg(feature = "vlan")]
fn first_vlan() -> VLanId {
    <VLanId as enum_map::Enum>::from_usize(0)
}

fn open_socket(i: usize, bind: IpAddr) -> Option<UdpSocket> {
    let name = config::SOCKET_NAMES[i];
    let port = config::SOCKET_PORTS[i];
    match UdpSocket::bind((bind, port)) {
        Ok(socket) => {
            socket
                .set_nonblocking(true)
                .expect("making a UDP socket nonblocking");
            match socket.local_addr() {
                Ok(addr) => eprintln!("net: socket {name} bound to {addr}"),
                Err(_) => eprintln!("net: socket {name} bound"),
            }
            Some(socket)
        }
        Err(e) => {
            let hint = if e.kind() == ErrorKind::PermissionDenied {
                "; ports below 1024 need privileges, so set port-offset in \
                 [tasks.net.config]"
            } else {
                ""
            };
            eprintln!(
                "net: socket {name} is unavailable: binding {bind} port \
                 {port}: {e}{hint}"
            );
            None
        }
    }
}

/// Whether a datagram is queued, without consuming it.
fn has_pending_datagram(socket: &UdpSocket) -> bool {
    let mut probe = [0u8; 1];
    match socket.peek_from(&mut probe) {
        Ok(_) => true,
        Err(e) => e.kind() != ErrorKind::WouldBlock,
    }
}

impl idl::InOrderNetImpl for ServerImpl {
    fn recv_packet(
        &mut self,
        msg: &RecvMessage,
        socket: SocketName,
        large_payload_behavior: LargePayloadBehavior,
        payload: Leased<W, [u8]>,
    ) -> Result<UdpMetadata, RequestError<RecvError>> {
        let i = socket as usize;
        Self::check_owner(msg, i).map_err(|e| e.map_runtime(|x| match x {}))?;
        loop {
            let Some(sock) = &self.sockets[i] else {
                return Err(RecvError::QueueEmpty.into());
            };
            let (n, from) = match sock.recv_from(&mut self.scratch) {
                Ok(received) => received,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    return Err(RecvError::QueueEmpty.into());
                }
                Err(e) => {
                    eprintln!(
                        "net: receiving on {}: {e}",
                        config::SOCKET_NAMES[i]
                    );
                    return Err(RecvError::QueueEmpty.into());
                }
            };
            #[cfg(feature = "vlan")]
            if !self.trusted(first_vlan(), i) {
                continue;
            }
            if payload.len() < n {
                match large_payload_behavior {
                    LargePayloadBehavior::Discard => continue,
                }
            }
            payload
                .write_range(0..n, &self.scratch[..n])
                .map_err(|_| RequestError::went_away())?;
            return Ok(self.metadata(from, n));
        }
    }

    fn send_packet(
        &mut self,
        msg: &RecvMessage,
        socket: SocketName,
        metadata: UdpMetadata,
        payload: Leased<R, [u8]>,
    ) -> Result<(), RequestError<SendError>> {
        let i = socket as usize;
        Self::check_owner(msg, i).map_err(|e| e.map_runtime(|x| match x {}))?;
        #[cfg(feature = "vlan")]
        if !self.trusted(metadata.vid, i) {
            // As on hardware: silently dropped.
            return Ok(());
        }
        let mut data = vec![0u8; payload.len()];
        payload
            .read_range(0..data.len(), &mut data)
            .map_err(|_| RequestError::went_away())?;

        let Some(sock) = &self.sockets[i] else {
            // An unbound socket is a network nobody is listening on.
            return Ok(());
        };
        let Some(dest) = sock
            .local_addr()
            .ok()
            .and_then(|local| self.destination(local, &metadata))
        else {
            return Ok(());
        };
        match sock.send_to(&data, dest) {
            Ok(_) => {
                self.waiting_to_send[i] = false;
                self.last_send_error[i] = None;
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                self.waiting_to_send[i] = true;
                Err(SendError::QueueFull.into())
            }
            Err(e) => {
                // Like a packet the stack couldn't route: dropped. Report
                // each new kind of failure once.
                if self.last_send_error[i] != Some(e.kind()) {
                    eprintln!(
                        "net: sending on {} to {dest}: {e}",
                        config::SOCKET_NAMES[i]
                    );
                    self.last_send_error[i] = Some(e.kind());
                }
                Ok(())
            }
        }
    }

    fn smi_read(
        &mut self,
        _msg: &RecvMessage,
        _phy: u8,
        _register: u8,
    ) -> Result<u16, RequestError<core::convert::Infallible>> {
        Ok(0)
    }

    fn smi_write(
        &mut self,
        _msg: &RecvMessage,
        _phy: u8,
        _register: u8,
        _value: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }

    fn read_phy_reg(
        &mut self,
        _msg: &RecvMessage,
        _port: u8,
        _page: u16,
        _reg: u8,
    ) -> Result<u16, RequestError<PhyError>> {
        Err(PhyError::NotImplemented.into())
    }

    fn write_phy_reg(
        &mut self,
        _msg: &RecvMessage,
        _port: u8,
        _page: u16,
        _reg: u8,
        _value: u16,
    ) -> Result<(), RequestError<PhyError>> {
        Err(PhyError::NotImplemented.into())
    }

    fn read_ksz8463_mac_count(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<usize, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    fn read_ksz8463_mac(
        &mut self,
        _msg: &RecvMessage,
        _i: u16,
    ) -> Result<KszMacTableEntry, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    fn read_ksz8463_reg(
        &mut self,
        _msg: &RecvMessage,
        _reg: u16,
    ) -> Result<u16, RequestError<KszError>> {
        Err(KszError::NotAvailable.into())
    }

    fn get_mac_address(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<MacAddress, RequestError<core::convert::Infallible>> {
        Ok(MacAddress(MAC_ADDRESS))
    }

    fn get_spare_mac_addresses(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<MacAddressBlock, RequestError<core::convert::Infallible>> {
        // No spares: the block is empty.
        Ok(MacAddressBlock::default())
    }

    fn management_link_status(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<ManagementLinkStatus, RequestError<MgmtError>> {
        Err(MgmtError::NotAvailable.into())
    }

    fn management_counters(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<ManagementCounters, RequestError<MgmtError>> {
        Err(MgmtError::NotAvailable.into())
    }

    #[cfg(feature = "vlan")]
    fn trust_vlan(
        &mut self,
        _msg: &RecvMessage,
        vid: VLanId,
        trust_until: u64,
    ) -> Result<(), RequestError<TrustError>> {
        if vid.cfg().always_trusted {
            return Err(TrustError::AlwaysTrusted.into());
        }
        self.trust[vid] = Trust::Until(trust_until);
        Ok(())
    }

    #[cfg(feature = "vlan")]
    fn distrust_vlan(
        &mut self,
        _msg: &RecvMessage,
        vid: VLanId,
    ) -> Result<(), RequestError<TrustError>> {
        if vid.cfg().always_trusted {
            return Err(TrustError::AlwaysTrusted.into());
        }
        self.trust[vid] = Trust::Distrust;
        Ok(())
    }

    #[cfg(not(feature = "vlan"))]
    fn trust_vlan(
        &mut self,
        _msg: &RecvMessage,
        _vid: VLanId,
        _trust_until: u64,
    ) -> Result<(), RequestError<TrustError>> {
        Err(TrustError::NoSuchVLAN.into())
    }

    #[cfg(not(feature = "vlan"))]
    fn distrust_vlan(
        &mut self,
        _msg: &RecvMessage,
        _vid: VLanId,
    ) -> Result<(), RequestError<TrustError>> {
        Err(TrustError::NoSuchVLAN.into())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::WAKE_TIMER_MASK
    }

    fn handle_notification(&mut self, bits: NotificationBits) {
        if bits.check_notification_mask(notifications::WAKE_TIMER_MASK) {
            self.wake_owners();
            self.arm_timer();
        }
    }
}

fn main() -> ! {
    let mut server = ServerImpl::new();
    server.arm_timer();
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod config {
    include!(concat!(env!("OUT_DIR"), "/host_net_config.rs"));
}

mod idl {
    use task_net_api::{
        KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
        MacAddressBlock, ManagementCounters, ManagementLinkStatus, MgmtError,
        PhyError, SocketName, UdpMetadata, VLanId,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
