// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_sprot_api::SprotError;
use gateway_messages::{
    sp_impl::{self, Sender},
    IgnitionCommand, MgsError, PowerState, SpComponent, UpdateId,
};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError,
};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use static_cell::ClaimOnceCell;
use task_control_plane_agent_api::MAX_INSTALLINATOR_IMAGE_ID_LEN;
use task_control_plane_agent_api::{
    BarcodeParseError, ControlPlaneAgentError, OxideIdentity, UartClient,
};
use task_net_api::{
    Address, LargePayloadBehavior, Net, RecvError, SendError, SocketName,
    UdpMetadata, VLanId,
};
use userlib::{sys_set_timer, task_slot};

mod inventory;
mod mgs_common;
mod update;

pub(crate) mod dump;

// If the build system enables multiple of the gimlet/sidecar/psc/minibar features, this
// sequence of `cfg_attr`s will trigger an unused_attributes warning.  We build
// everything with -Dunused_attributes, which will catch any such build system
// misconfiguration.
#[cfg_attr(feature = "compute-sled", path = "mgs_compute_sled.rs")]
#[cfg_attr(feature = "sidecar", path = "mgs_sidecar.rs")]
#[cfg_attr(feature = "psc", path = "mgs_psc.rs")]
#[cfg_attr(feature = "minibar", path = "mgs_minibar.rs")]
mod mgs_handler;

use self::mgs_handler::MgsHandler;

task_slot!(JEFE, jefe);
task_slot!(NET, net);
task_slot!(SYS, sys);

#[allow(dead_code)] // Not all cases are used by all variants
#[derive(Clone, Copy, PartialEq, ringbuf::Count)]
enum Log {
    #[count(skip)]
    Empty,
    BarcodeParseError(BarcodeParseError),
    Rx(UdpMetadata),
    SendError(SendError),
    MgsMessage(#[count(children)] MgsMessage),
    UsartTxFull {
        remaining: usize,
    },
    UsartRxOverrun,
    UsartRxBufferDataDropped {
        num_bytes: u64,
    },
    SerialConsoleSend {
        buffered: usize,
    },
    UpdatePartial {
        bytes_written: u32,
    },
    UpdateComplete,
    HostFlashSectorsErased {
        num_sectors: usize,
    },
    ExpectedRspTimeout,
    RotReset(SprotError),
    SprotCabooseSize(u32),
    ReadCaboose(u32, usize),
    GotCabooseChunk([u8; 4]),
    ReadRotPage,
    IpcRequest(#[count(children)] IpcRequest),
    VpdLockStatus,
}

// This enum does not define the actual MGS protocol - it is only used in the
// `Log` enum above (which itself is only used by our ringbuf logs). The MGS
// protocol is defined in the `gateway-messages` crate (which is shared with
// MGS proper and other tools like `sp-sim` in the omicron repository).
#[derive(Clone, Copy, PartialEq, ringbuf::Count)]
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
    SerialConsoleKeepAlive,
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
    ComponentGetActiveSlot {
        component: SpComponent,
    },
    ComponentSetActiveSlot {
        component: SpComponent,
        slot: u16,
        persist: bool,
    },
    ComponentGetPersistentSlot {
        component: SpComponent,
    },
    SerialConsoleBreak,
    SendHostNmi,
    SetIpccKeyValue {
        key: u8,
        value_len: usize,
    },
    ReadRotPage,
    VpdLockStatus,
    VersionedRotBootInfo {
        version: u8,
    },
    ReadHostFlash {
        addr: u32,
    },
    StartHostFlashHash {
        slot: u16,
    },
    GetHostFlashHash {
        slot: u16,
    },
}

// This enum does not define the actual IPC protocol - it is only used in the
// `Log` enum above (which itself is only used by our ringbuf logs).
#[derive(Clone, Copy, PartialEq, ringbuf::Count)]
enum IpcRequest {
    FetchHostPhase2Data,
    GetHostPhase2Data,
    GetStartupOptions,
    SetStartupOptions(HostStartupOptions),
    Identity,
    #[cfg(feature = "compute-sled")]
    GetInstallinatorImageId,
    #[cfg(feature = "compute-sled")]
    GetUartClient,
    #[cfg(feature = "compute-sled")]
    SetHumilityUartClient(#[count(children)] UartClient),
    #[cfg(feature = "compute-sled")]
    UartRead(usize),
    #[cfg(feature = "compute-sled")]
    UartWrite(usize),
}

counted_ringbuf!(Log, 16, Log::Empty);

#[derive(Copy, Clone, PartialEq, ringbuf::Count)]
enum CriticalEvent {
    Empty,
    /// We have received a network request to change power states. This record
    /// logs the sender, in case the request was unexpected, and the target
    /// state.
    SetPowerState {
        sender: Sender<VLanId>,
        power_state: PowerState,
        ticks_since_boot: u64,
    },
}

// This ringbuf exists to record critical events _only_ and thus not get
// overwritten by chatter in the debug/trace-style messages.
counted_ringbuf!(CRITICAL, CriticalEvent, 16, CriticalEvent::Empty);

const SOCKET: SocketName = SocketName::control_plane_agent;

#[export_name = "main"]
fn main() {
    let mut server = ServerImpl::claim_static_resources();

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        sys_set_timer(server.timer_deadline(), notifications::TIMER_MASK);
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    mgs_handler: MgsHandler,
    net_handler: NetHandler,
}

impl ServerImpl {
    fn claim_static_resources() -> Self {
        let net_handler = NetHandler::claim_static_resources();
        let base_mac_address = net_handler.net.get_mac_address();
        Self {
            mgs_handler: MgsHandler::claim_static_resources(base_mac_address),
            net_handler,
        }
    }

    fn timer_deadline(&self) -> Option<u64> {
        self.mgs_handler.timer_deadline()
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        #[cfg(not(feature = "minibar"))]
        let mask = notifications::SOCKET_MASK
            | notifications::USART_IRQ_MASK
            | notifications::TIMER_MASK;

        // Minibar does not configure USART for serial console, so omit it
        // from the mask.
        #[cfg(feature = "minibar")]
        let mask = notifications::SOCKET_MASK | notifications::TIMER_MASK;

        mask
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        #[cfg(not(feature = "minibar"))]
        if bits.check_notification_mask(notifications::USART_IRQ_MASK) {
            self.mgs_handler.drive_usart();
        }

        if bits.has_timer_fired(notifications::TIMER_MASK) {
            self.mgs_handler.handle_timer_fired();
        }

        if bits.check_notification_mask(notifications::SOCKET_MASK)
            || self.net_handler.packet_to_send.is_some()
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
        ringbuf_entry!(Log::IpcRequest(IpcRequest::FetchHostPhase2Data));
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
        ringbuf_entry!(Log::IpcRequest(IpcRequest::GetHostPhase2Data));
        self.mgs_handler
            .get_host_phase2_data(image_hash, offset, data)
    }

    fn get_startup_options(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::GetStartupOptions));
        self.mgs_handler.startup_options_impl()
    }

    fn set_startup_options(
        &mut self,
        _msg: &userlib::RecvMessage,
        startup_options: u64,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        let startup_options = HostStartupOptions::from_bits(startup_options)
            .ok_or(ControlPlaneAgentError::InvalidStartupOptions)?;
        ringbuf_entry!(Log::IpcRequest(IpcRequest::SetStartupOptions(
            startup_options
        )));
        self.mgs_handler.set_startup_options_impl(startup_options)
    }

    fn identity(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<OxideIdentity, RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::Identity));
        Ok(self.mgs_handler.identity())
    }

    #[cfg(feature = "compute-sled")]
    fn get_installinator_image_id(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: LenLimit<
            Leased<idol_runtime::W, [u8]>,
            MAX_INSTALLINATOR_IMAGE_ID_LEN,
        >,
    ) -> Result<usize, RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::GetInstallinatorImageId));
        let image_id = self.mgs_handler.installinator_image_id();
        if image_id.len() > data.len() {
            // `image_id` is at most `MAX_INSTALLINATOR_IMAGE_ID_LEN`; if our
            // client didn't send us that much space, fault them.
            Err(RequestError::Fail(ClientError::BadLease))
        } else {
            data.write_range(0..image_id.len(), image_id)
                .map_err(|()| RequestError::went_away())?;
            Ok(image_id.len())
        }
    }

    #[cfg(not(feature = "compute-sled"))]
    fn get_installinator_image_id(
        &mut self,
        _msg: &userlib::RecvMessage,
        _data: LenLimit<
            Leased<idol_runtime::W, [u8]>,
            MAX_INSTALLINATOR_IMAGE_ID_LEN,
        >,
    ) -> Result<usize, RequestError<core::convert::Infallible>> {
        // Non-gimlets should never request this function; fault them.
        //
        // TODO can we remove this op from our idol specification entirely for
        // non-gimlets?
        Err(RequestError::Fail(ClientError::BadMessageContents))
    }

    #[cfg(feature = "compute-sled")]
    fn get_uart_client(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<UartClient, RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::GetUartClient));
        Ok(self.mgs_handler.uart_client())
    }

    #[cfg(feature = "compute-sled")]
    fn set_humility_uart_client(
        &mut self,
        _msg: &userlib::RecvMessage,
        attach: bool,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        let client = if attach {
            UartClient::Humility
        } else {
            UartClient::Mgs
        };
        ringbuf_entry!(Log::IpcRequest(IpcRequest::SetHumilityUartClient(
            client
        )));
        Ok(self.mgs_handler.set_uart_client(client)?)
    }

    #[cfg(feature = "compute-sled")]
    fn uart_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::UartRead(data.len())));
        self.mgs_handler.uart_read(data)
    }

    #[cfg(feature = "compute-sled")]
    fn uart_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: Leased<idol_runtime::R, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        ringbuf_entry!(Log::IpcRequest(IpcRequest::UartWrite(data.len())));
        self.mgs_handler.uart_write(data)
    }

    #[cfg(not(feature = "compute-sled"))]
    fn get_uart_client(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<UartClient, RequestError<core::convert::Infallible>> {
        // This operation is idempotent and infallible, but we don't actually
        // have a uart. Just always report "MGS" (the default for gimlets).
        Ok(UartClient::Mgs)
    }

    #[cfg(not(feature = "compute-sled"))]
    fn set_humility_uart_client(
        &mut self,
        _msg: &userlib::RecvMessage,
        _attach: bool,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        Err(RequestError::from(
            ControlPlaneAgentError::OperationUnsupported,
        ))
    }

    #[cfg(not(feature = "compute-sled"))]
    fn uart_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        _data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        Err(RequestError::from(
            ControlPlaneAgentError::OperationUnsupported,
        ))
    }

    #[cfg(not(feature = "compute-sled"))]
    fn uart_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        _data: Leased<idol_runtime::R, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        Err(RequestError::from(
            ControlPlaneAgentError::OperationUnsupported,
        ))
    }
}

struct NetHandler {
    net: Net,
    tx_buf: &'static mut NetBuf,
    rx_buf: &'static mut NetBuf,
    packet_to_send: Option<UdpMetadata>,
}

type NetBuf = [u8; gateway_messages::MAX_SERIALIZED_SIZE];

impl NetHandler {
    fn claim_static_resources() -> Self {
        let [tx_buf, rx_buf] = {
            static BUFS: ClaimOnceCell<[NetBuf; 2]> = ClaimOnceCell::new(
                [[0; gateway_messages::MAX_SERIALIZED_SIZE]; 2],
            );
            BUFS.claim()
        };
        Self {
            net: Net::from(NET.get_task_id()),
            tx_buf,
            rx_buf,
            packet_to_send: None,
        }
    }

    fn run_until_blocked(&mut self, mgs_handler: &mut MgsHandler) {
        // If we get `ServerRestarted` from the net task when attempting to
        // send, we'll immediately retry. However, we still want to put a limit
        // on this in case `net` is in a crash loop - we won't be able to do
        // much, but we can be nicer than busy waiting for it to come back (and
        // handle any IPC requests made of us in the meantime). If we hit this
        // max retry count, we'll consider ourselves blocked and return. We'll
        // get to try again the next time we're interrupted (hopefully by `net`
        // coming back).
        const MAX_NET_RESTART_RETRIES: usize = 3;
        let mut net_restart_retries = 0;

        loop {
            // Try to send first.
            if let Some(meta) = self.packet_to_send.take() {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_buf[..meta.size as usize],
                ) {
                    Ok(()) => (),
                    Err(err @ SendError::ServerRestarted) => {
                        ringbuf_entry!(Log::SendError(err));

                        // `net` died; re-enqueue this packet.
                        self.packet_to_send = Some(meta);

                        // Either immediately retry or give up until the next
                        // time we get a notification.
                        if net_restart_retries < MAX_NET_RESTART_RETRIES {
                            net_restart_retries += 1;
                            continue;
                        } else {
                            return;
                        }
                    }
                    Err(err @ SendError::QueueFull) => {
                        ringbuf_entry!(Log::SendError(err));

                        // "Re-enqueue" packet and return; we'll wait until
                        // `net` wakes us again to retry.
                        self.packet_to_send = Some(meta);
                        return;
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
                Err(RecvError::QueueEmpty | RecvError::ServerRestarted) => {
                    // In the restart case, there may in fact be packets waiting
                    // for us in the net stack. We'll handle them next time
                    // through the loop when we get to recv_packet.
                    return;
                }
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
        let addr = gateway_messages::sp_impl::SocketAddrV6 {
            ip: addr.into(),
            port: meta.port,
        };

        // Hand off to `sp_impl` to handle deserialization, calling our
        // `MgsHandler` implementation, and serializing the response we should
        // send into `self.tx_buf`.
        assert!(self.packet_to_send.is_none());
        let sender = Sender {
            addr,
            vid: meta.vid,
        };
        if let Some(n) = sp_impl::handle_message(
            sender,
            &self.rx_buf[..meta.size as usize],
            mgs_handler,
            self.tx_buf,
        ) {
            meta.size = n as u32;
            self.packet_to_send = Some(meta);
        }
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
        ControlPlaneAgentError, HostStartupOptions, OxideIdentity, UartClient,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
