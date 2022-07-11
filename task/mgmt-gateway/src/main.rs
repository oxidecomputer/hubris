// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use gateway_messages::sp_impl;
use gateway_messages::sp_impl::Error as MgsDispatchError;
use gateway_messages::IgnitionCommand;
use gateway_messages::Request;
use gateway_messages::SerializedSize;
use gateway_messages::SpMessage;
use gateway_messages::SpPort;
use ringbuf::ringbuf;
use ringbuf::ringbuf_entry;
use task_net_api::Net;
use task_net_api::NetError;
use task_net_api::SocketName;
use task_net_api::UdpMetadata;
use userlib::sys_recv_closed;
use userlib::task_slot;
use userlib::TaskId;
use userlib::UnwrapLite;

mod mgs_handler;

use self::mgs_handler::MgsHandler;

task_slot!(NET, net);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Log {
    Empty,
    Rx(UdpMetadata),
    DispatchError(MgsDispatchError),
    SendError(NetError),
    MgsMessage(MgsMessage),
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
    SerialConsoleWrite {
        length: u16,
    },
}

ringbuf!(Log, 16, Log::Empty);

// Must match app.toml!
const NET_IRQ: u32 = 1;

const SOCKET: SocketName = SocketName::mgmt_gateway;

#[export_name = "main"]
fn main() {
    let mut net_handler = NetHandler::default();
    let mut note = NET_IRQ;

    loop {
        if (note & NET_IRQ) != 0 {
            net_handler.run_until_blocked();
        }

        note = sys_recv_closed(&mut [], NET_IRQ, TaskId::KERNEL)
            .unwrap_lite()
            .operation;
    }
}

struct NetHandler {
    net: Net,
    rx_buf: [u8; Request::MAX_SIZE],
    tx_buf: [u8; SpMessage::MAX_SIZE],
    packet_to_send: Option<UdpMetadata>,
}

impl Default for NetHandler {
    fn default() -> Self {
        Self {
            net: Net::from(NET.get_task_id()),
            rx_buf: [0; Request::MAX_SIZE],
            tx_buf: [0; SpMessage::MAX_SIZE],
            packet_to_send: None,
        }
    }
}

impl NetHandler {
    fn run_until_blocked(&mut self) {
        loop {
            // Try to send first.
            if let Some(meta) = self.packet_to_send.take() {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_buf[..meta.size as usize],
                ) {
                    Ok(()) => (),
                    Err(err) => {
                        ringbuf_entry!(Log::SendError(err));

                        // Re-enqueue packet and return; we'll wait to be awoken
                        // by the net task when it has room for us to send.
                        //
                        // TODO Should we drop packets for non-"out of space"
                        // errors? Need to fix net task returning QueueEmpty for
                        // arbitrary errors.
                        self.packet_to_send = Some(meta);
                        return;
                    }
                }
            }

            // We have nothing to send (possibly because we just successfully
            // sent a message we previously enqueued), so check for an incoming
            // packet.
            match self.net.recv_packet(SOCKET, &mut self.rx_buf) {
                Ok(meta) => {
                    self.handle_received_packet(meta);
                }
                Err(NetError::QueueEmpty) => {
                    return;
                }
                Err(NetError::NotYours) => panic!(),
                Err(NetError::InvalidVLan) => panic!(),
            }
        }
    }

    fn handle_received_packet(&mut self, mut meta: UdpMetadata) {
        ringbuf_entry!(Log::Rx(meta));

        let addr = match meta.addr {
            task_net_api::Address::Ipv6(addr) => addr,
        };
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
            &mut MgsHandler,
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
