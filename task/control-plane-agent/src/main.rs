// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use gateway_messages::{
    sp_impl, IgnitionCommand, MgsError, PowerState, SpComponent, SpPort,
    UpdateId,
};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, NotificationHandler, RequestError};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use task_control_plane_agent_api::{ControlPlaneAgentError, Identity};
use task_net_api::{
    Address, LargePayloadBehavior, Net, RecvError, SendError, SocketName,
    UdpMetadata,
};
use userlib::{sys_set_timer, task_slot};

mod inventory;
mod mgs_common;
mod update;

// If the build system enables multiple of the gimlet/sidecar/psc features, this
// sequence of `cfg_attr`s will trigger an unused_attributes warning.  We build
// everything with -Dunused_attributes, which will catch any such build system
// misconfiguration.
#[cfg_attr(feature = "gimlet", path = "mgs_gimlet.rs")]
#[cfg_attr(feature = "sidecar", path = "mgs_sidecar.rs")]
#[cfg_attr(feature = "psc", path = "mgs_psc.rs")]
mod mgs_handler;

use self::mgs_handler::MgsHandler;

task_slot!(JEFE, jefe);
task_slot!(NET, net);
task_slot!(SYS, sys);

#[cfg(feature = "vpd-identity")]
task_slot!(I2C, i2c_driver);

#[allow(dead_code)] // Not all cases are used by all variants
#[derive(Debug, Clone, Copy, PartialEq)]
enum Log {
    Empty,
    Wake(u32),
    Rx(UdpMetadata),
    SendError(SendError),
    MgsMessage(MgsMessage),
    UsartTx { num_bytes: usize },
    UsartTxFull { remaining: usize },
    UsartRx { num_bytes: usize },
    UsartRxOverrun,
    UsartRxBufferDataDropped { num_bytes: u64 },
    SerialConsoleSend { buffered: usize },
    UpdatePartial { bytes_written: u32 },
    UpdateComplete,
    HostFlashSectorsErased { num_sectors: usize },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MgsMessage {
    Discovery,
    IgnitionState {
        target: u8,
    },
    BulkIgnitionState {
        offset: u32,
    },
    IgnitionLinkEvents {
        target: u8,
    },
    BulkIgnitionLinkEvents {
        offset: u32,
    },
    ClearIgnitionLinkEvents,
    IgnitionCommand {
        target: u8,
        command: IgnitionCommand,
    },
    SpState,
    SerialConsoleAttach,
    SerialConsoleWrite {
        offset: u64,
        length: u16,
    },
    SerialConsoleDetach,
    UpdatePrepare {
        component: SpComponent,
        id: UpdateId,
        length: u32,
        slot: u16,
    },
    UpdateChunk {
        component: SpComponent,
        offset: u32,
    },
    UpdateStatus {
        component: SpComponent,
    },
    UpdateAbort {
        component: SpComponent,
    },
    GetPowerState,
    SetPowerState(PowerState),
    ResetPrepare,
    Inventory,
    HostPhase2Data {
        hash: [u8; 32],
        offset: u64,
        data_len: usize,
    },
    MgsError {
        message_id: u32,
        err: MgsError,
    },
    GetStartupOptions,
    SetStartupOptions(gateway_messages::StartupOptions),
    ComponentDetails {
        component: SpComponent,
    },
    ComponentClearStatus {
        component: SpComponent,
    },
}

ringbuf!(Log, 16, Log::Empty);

// Must match app.toml!
const NET_IRQ: u32 = 1 << 0;
const USART_IRQ: u32 = 1 << 1;

// Must not conflict with IRQs above!
const TIMER_IRQ: u32 = 1 << 2;

const SOCKET: SocketName = SocketName::control_plane_agent;

#[export_name = "main"]
fn main() {
    let mut server = ServerImpl::claim_static_resources();

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        sys_set_timer(server.timer_deadline(), TIMER_IRQ);
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    mgs_handler: MgsHandler,
    net_handler: NetHandler,
}

impl ServerImpl {
    fn claim_static_resources() -> Self {
        Self {
            mgs_handler: MgsHandler::claim_static_resources(),
            net_handler: NetHandler::claim_static_resources(),
        }
    }

    fn timer_deadline(&self) -> Option<u64> {
        self.mgs_handler.timer_deadline()
    }

    #[cfg(feature = "vpd-identity")]
    fn identity_from_vpd(&self) -> Option<Identity> {
        use core::{mem, str};
        use zerocopy::{AsBytes, FromBytes};

        #[derive(Debug, Clone, Copy, PartialEq, Eq, AsBytes, FromBytes)]
        #[repr(C, packed)]
        pub struct BarcodeVpd {
            pub version: [u8; 4],
            pub delim0: u8,
            // VPD omits the hyphen 3 bytes into the part number, which we add
            // back into `Identity` below, hence the "minus 1".
            pub part_number: [u8; Identity::PART_NUMBER_LEN - 1],
            pub delim1: u8,
            pub revision: [u8; 3],
            pub delim2: u8,
            pub serial: [u8; Identity::SERIAL_LEN],
        }
        static_assertions::const_assert_eq!(mem::size_of::<BarcodeVpd>(), 31);

        let i2c_task = I2C.get_task_id();
        let barcode: BarcodeVpd =
            drv_local_vpd::read_config(i2c_task, *b"BARC").ok()?;

        // Check expected values of fields, since `barcode` was created
        // via zerocopy (i.e., memcopying a byte array).
        if barcode.delim0 != b':'
            || barcode.delim1 != b':'
            || barcode.delim2 != b':'
        {
            return None;
        }

        // Allow `0` or `O` for the first byte of the version (which isn't
        // part of the identity we return, but tells us the format of the
        // barcode string itself).
        if barcode.version != *b"0XV1" && barcode.version != *b"OXV1" {
            return None;
        }

        let mut identity = Identity::default();

        // Parse revision into a u32
        identity.revision =
            str::from_utf8(&barcode.revision).ok()?.parse().ok()?;

        // Insert a hyphen 3 characters into the part number (which we know we
        // have room for based on the size of the `part_number` fields)
        identity.part_number[..3].copy_from_slice(&barcode.part_number[..3]);
        identity.part_number[3] = b'-';
        identity.part_number[4..][..barcode.part_number.len() - 3]
            .copy_from_slice(&barcode.part_number[3..]);

        // Copy the serial as-is.
        identity.serial[..barcode.serial.len()]
            .copy_from_slice(&barcode.serial);

        Some(identity)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        NET_IRQ | USART_IRQ | TIMER_IRQ
    }

    fn handle_notification(&mut self, bits: u32) {
        ringbuf_entry!(Log::Wake(bits));

        if (bits & USART_IRQ) != 0 {
            self.mgs_handler.drive_usart();
        }

        if (bits & TIMER_IRQ) != 0 {
            self.mgs_handler.handle_timer_fired();
        }

        if (bits & NET_IRQ) != 0
            || self.mgs_handler.wants_to_send_packet_to_mgs()
        {
            self.net_handler.run_until_blocked(&mut self.mgs_handler);
        }
    }
}

impl idl::InOrderControlPlaneAgentImpl for ServerImpl {
    fn fetch_host_phase2_data(
        &mut self,
        msg: &userlib::RecvMessage,
        image_hash: [u8; 32],
        offset: u64,
        notification_bit: u8,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        self.mgs_handler.fetch_host_phase2_data(
            msg,
            image_hash,
            offset,
            notification_bit,
        )
    }

    fn get_host_phase2_data(
        &mut self,
        _msg: &userlib::RecvMessage,
        image_hash: [u8; 32],
        offset: u64,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        self.mgs_handler
            .get_host_phase2_data(image_hash, offset, data)
    }

    fn get_startup_options(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        self.mgs_handler.startup_options()
    }

    fn set_startup_options(
        &mut self,
        _msg: &userlib::RecvMessage,
        startup_options: u64,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        let startup_options = HostStartupOptions::from_bits(startup_options)
            .ok_or(ControlPlaneAgentError::InvalidStartupOptions)?;

        self.mgs_handler.set_startup_options(startup_options)
    }

    fn identity(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<Identity, RequestError<core::convert::Infallible>> {
        #[cfg(feature = "vpd-identity")]
        let id = self.identity_from_vpd().unwrap_or_default();

        #[cfg(not(feature = "vpd-identity"))]
        let id = Identity::default();

        Ok(id)
    }
}

struct NetHandler {
    net: Net,
    tx_buf: &'static mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    rx_buf: &'static mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    packet_to_send: Option<UdpMetadata>,
}

impl NetHandler {
    fn claim_static_resources() -> Self {
        let (tx_buf, rx_buf) = mutable_statics! {
            static mut NET_TX_BUF: [u8; gateway_messages::MAX_SERIALIZED_SIZE] =
                [|| 0; _];

            static mut NET_RX_BUF: [u8; gateway_messages::MAX_SERIALIZED_SIZE] =
                [|| 0; _];
        };
        Self {
            net: Net::from(NET.get_task_id()),
            tx_buf,
            rx_buf,
            packet_to_send: None,
        }
    }

    fn run_until_blocked(&mut self, mgs_handler: &mut MgsHandler) {
        loop {
            // Try to send first.
            if let Some(meta) = self.packet_to_send.take() {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_buf[..meta.size as usize],
                ) {
                    Ok(()) => (),
                    Err(err @ SendError::QueueFull) => {
                        ringbuf_entry!(Log::SendError(err));

                        // "Re-enqueue" packet and return; we'll wait until
                        // `net` wakes us again to retry.
                        self.packet_to_send = Some(meta);
                        return;
                    }
                    Err(err) => {
                        // Some other (fatal?) error occurred; should we panic?
                        // For now, just discard the packet we wanted to send.
                        ringbuf_entry!(Log::SendError(err));
                    }
                }
            }

            // Do we need to send a packet to MGS?
            if let Some(meta) = mgs_handler.packet_to_mgs(self.tx_buf) {
                self.packet_to_send = Some(meta);

                // Loop back to send.
                continue;
            }

            // All sending is complete; check for an incoming packet.
            match self.net.recv_packet(
                SOCKET,
                LargePayloadBehavior::Discard,
                self.rx_buf,
            ) {
                Ok(meta) => {
                    self.handle_received_packet(meta, mgs_handler);
                }
                Err(RecvError::QueueEmpty) => {
                    return;
                }
                Err(RecvError::NotYours | RecvError::Other) => panic!(),
            }
        }
    }

    fn handle_received_packet(
        &mut self,
        mut meta: UdpMetadata,
        mgs_handler: &mut MgsHandler,
    ) {
        ringbuf_entry!(Log::Rx(meta));

        let Address::Ipv6(addr) = meta.addr;
        let sender = gateway_messages::sp_impl::SocketAddrV6 {
            ip: addr.into(),
            port: meta.port,
        };

        // Hand off to `sp_impl` to handle deserialization, calling our
        // `MgsHandler` implementation, and serializing the response we should
        // send into `self.tx_buf`.
        assert!(self.packet_to_send.is_none());
        if let Some(n) = sp_impl::handle_message(
            sender,
            sp_port_from_udp_metadata(&meta),
            &self.rx_buf[..meta.size as usize],
            mgs_handler,
            self.tx_buf,
        ) {
            meta.size = n as u32;
            self.packet_to_send = Some(meta);
        }
    }
}

fn sp_port_from_udp_metadata(meta: &UdpMetadata) -> SpPort {
    use task_net_api::VLAN_RANGE;
    assert!(VLAN_RANGE.contains(&meta.vid));
    assert_eq!(VLAN_RANGE.len(), 2);

    match meta.vid - VLAN_RANGE.start {
        0 => SpPort::One,
        1 => SpPort::Two,
        _ => unreachable!(),
    }
}

#[allow(dead_code)]
fn vlan_id_from_sp_port(port: SpPort) -> u16 {
    use task_net_api::VLAN_RANGE;
    assert_eq!(VLAN_RANGE.len(), 2);

    match port {
        SpPort::One => VLAN_RANGE.start,
        SpPort::Two => VLAN_RANGE.start + 1,
    }
}

#[allow(dead_code)]
const fn usize_max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

mod idl {
    use task_control_plane_agent_api::{
        ControlPlaneAgentError, HostStartupOptions, Identity,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
