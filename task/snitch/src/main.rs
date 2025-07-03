// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use counters::{count, counters, Count};
use gateway_ereport_messages::Request;
use task_net_api::{
    LargePayloadBehavior, Net, RecvError, SendError, SocketName,
};
use task_packrat_api::Packrat;
use userlib::{sys_recv_notification, task_slot};
use zerocopy::TryFromBytes;

task_slot!(NET, net);
task_slot!(PACKRAT, packrat);

#[derive(Count, Copy, Clone)]
enum Event {
    RecvPacket,
    RequestRejected,
    Respond,
}

struct StaticBufs {
    rx_buf: [u8; REQ_SZ],
    tx_buf: [u8; UDP_PACKET_SZ],
}

const REQ_SZ: usize = core::mem::size_of::<Request>();
const UDP_PACKET_SZ: usize = 1024;

counters!(Event);

#[export_name = "main"]
fn main() -> ! {
    let net = Net::from(NET.get_task_id());
    let packrat = Packrat::from(PACKRAT.get_task_id());

    const SOCKET: SocketName = SocketName::ereport;

    let StaticBufs {
        ref mut rx_buf,
        ref mut tx_buf,
    } = {
        static BUFS: static_cell::ClaimOnceCell<StaticBufs> =
            static_cell::ClaimOnceCell::new(StaticBufs {
                rx_buf: [0u8; REQ_SZ],
                tx_buf: [0u8; UDP_PACKET_SZ],
            });
        BUFS.claim()
    };

    loop {
        let meta = match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_buf[..],
        ) {
            Ok(meta) => meta,
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_notification(notifications::SOCKET_MASK);
                continue;
            }
            Err(RecvError::ServerRestarted) => {
                // `net` restarted; just retry.
                continue;
            }
        };

        // Okay, we got a packet!
        count!(Event::RecvPacket);
        let request =
            match Request::try_ref_from_bytes(&rx_buf[..meta.size as usize]) {
                Ok(req) => req,
                Err(_) => {
                    // We ignore malformatted, truncated, etc. packets.
                    count!(Event::RequestRejected);
                    continue;
                }
            };

        let size = match request {
            Request::V0(req) => packrat.read_ereports(
                req.request_id,
                req.restart_id,
                req.start_ena,
                req.committed_ena()
                    .copied()
                    .unwrap_or(gateway_ereport_messages::Ena::NONE),
                &mut tx_buf[..],
            ),
        };

        // With the response packet prepared, we may need to attempt
        // sending more than once.
        loop {
            match net.send_packet(SOCKET, meta, &tx_buf[..size]) {
                Ok(()) => {
                    count!(Event::Respond);
                    break;
                }
                // If `net` just restarted, immediately retry our send.
                Err(SendError::ServerRestarted) => continue,
                // If our tx queue is full, wait for space. This is the
                // same notification we get for incoming packets, so we
                // might spuriously wake up due to an incoming packet
                // (which we can't service anyway because we are still
                // waiting to respond to a previous request); once we
                // finally succeed in sending we'll peel any queued
                // packets off our recv queue at the top of our main
                // loop.
                Err(SendError::QueueFull) => {
                    sys_recv_notification(notifications::SOCKET_MASK);
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
