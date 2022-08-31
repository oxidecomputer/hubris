// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use gateway_messages::{
    sp_impl, sp_impl::Error as MgsDispatchError, IgnitionCommand, SpPort,
};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use task_net_api::{
    Address, LargePayloadBehavior, Net, RecvError, SendError, SocketName,
    UdpMetadata,
};
use userlib::{sys_recv_closed, task_slot, TaskId, UnwrapLite};

mod mgs_handler;

use self::mgs_handler::MgsHandler;

type SerializedMessageBuf = [u8; gateway_messages::MAX_SERIALIZED_SIZE];

task_slot!(JEFE, jefe);
task_slot!(NET, net);
task_slot!(SYS, sys);
task_slot!(UPDATE_SERVER, update_server);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Log {
    Empty,
    Wake(u32),
    Rx(UdpMetadata),
    DispatchError(MgsDispatchError),
    SendError(SendError),
    MgsMessage(MgsMessage),
    UsartTx { num_bytes: usize },
    UsartTxFull { remaining: usize },
    UsartRx { num_bytes: usize },
    UsartRxOverrun,
    UsartRxBufferDataDropped { num_bytes: u64 },
    SerialConsoleSend { buffered: usize },
    UpdatePartial { bytes_written: usize },
    UpdateComplete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MgsMessage {
    Discovery,
    IgnitionState {
        target: u8,
    },
    BulkIgnitionState,
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
    UpdateStart {
        length: u32,
    },
    UpdateChunk {
        offset: u32,
    },
    SysResetPrepare,
}

ringbuf!(Log, 16, Log::Empty);

// Must match app.toml!
const NET_IRQ: u32 = 1 << 0;
const USART_IRQ: u32 = 1 << 1;

// Must not conflict with IRQs above!
const TIMER_IRQ: u32 = 1 << 2;

const SOCKET: SocketName = SocketName::mgmt_gateway;

#[export_name = "main"]
fn main() {
    let mut mgs_handler = MgsHandler::claim_static_resources();
    let mut net_handler = NetHandler::new(claim_net_bufs_static());

    loop {
        let note = sys_recv_closed(
            &mut [],
            NET_IRQ | USART_IRQ | TIMER_IRQ,
            TaskId::KERNEL,
        )
        .unwrap_lite()
        .operation;
        ringbuf_entry!(Log::Wake(note));

        if (note & USART_IRQ) != 0 {
            mgs_handler.drive_usart();
        }

        if (note & NET_IRQ) != 0 || mgs_handler.wants_to_send_packet_to_mgs() {
            net_handler.run_until_blocked(&mut mgs_handler);
        }
    }
}

struct NetHandler {
    net: Net,
    tx_buf: &'static mut SerializedMessageBuf,
    rx_buf: &'static mut SerializedMessageBuf,
    packet_to_send: Option<UdpMetadata>,
}

impl NetHandler {
    fn new(buffers: &'static mut [SerializedMessageBuf; 2]) -> Self {
        let [tx_buf, rx_buf] = buffers;
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
        match sp_impl::handle_message(
            sender,
            sp_port_from_udp_metadata(&meta),
            &self.rx_buf[..meta.size as usize],
            mgs_handler,
            &mut self.tx_buf,
        ) {
            Ok(n) => {
                meta.size = n as u32;
                assert!(self.packet_to_send.is_none());
                self.packet_to_send = Some(meta);
            }
            Err(err) => ringbuf_entry!(Log::DispatchError(err)),
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

fn vlan_id_from_sp_port(port: SpPort) -> u16 {
    use task_net_api::VLAN_RANGE;
    assert_eq!(VLAN_RANGE.len(), 2);

    match port {
        SpPort::One => VLAN_RANGE.start,
        SpPort::Two => VLAN_RANGE.start + 1,
    }
}

/// Grabs reference to a static array sized to hold an incoming message. Can
/// only be called once!
fn claim_net_bufs_static() -> &'static mut [SerializedMessageBuf; 2] {
    mutable_statics! {
        static mut BUFS: [SerializedMessageBuf; 2] =
            [[0; gateway_messages::MAX_SERIALIZED_SIZE]; _];
    }
}
