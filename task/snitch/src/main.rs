// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! the snitch: ereport evacuation
//!
//! The snitch is the component of the ereport subsystem responsible for
//! evacuating ereports over the network, as described in [RFD 545 § 4.4]. The
//! snitch task does not store ereports in its memory; this is `packrat`'s
//! responsibility. Instead, the snitch's role is to receive requests for
//! ereports over the management network, read them from packrat, and forward
//! them to the requesting management gateway.
//!
//! This split is necessary because, in order to communicate over the management
//! network, the snitch must run at a relatively low priority: in particular, it
//! must be lower than that of the `net` task, of which it is a client. Since we
//! would like a variety of tasks to be able to *report* errors through the
//! ereport subsystem, the task responsible for aggregating ereports in memory
//! must run at a high priority, so that as many other tasks as possible may act
//! as clients of it. Furthermore, we would like to include the SP's VPD
//! identity in ereport messages, and this is already stored in packrat.
//! Therefore, we separate the responsibility for storing ereports from the
//! responsibility for sending them over the network.
//!
//! Due to this separation of responsibilities, the snitch task is fairly
//! simple. It receives packets sent to the ereport socket, interprets the
//! request message, and forwards the request to packrat. Any ereports sent back
//! by packrat are sent in response to the request. The snitch ends up being a
//! pretty dumb proxy: as the response packet is encoded by packrat; all we end
//! up doing is taking the bytes received from packrat and stuffing them into
//! the socket's send queue. The real purpose of this thing is just to serve as
//! a trampoline between the high priority level of packrat and a priority level
//! lower than that of the net task.
//!
//! [RFD 545 § 4.4]: https://rfd.shared.oxide.computer/rfd/0545#_evacuation
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
    ReadError(#[count(children)] task_packrat_api::EreportReadError),
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
            Request::V0(req) => match packrat.read_ereports(
                req.request_id,
                req.restart_id,
                req.start_ena,
                req.limit,
                req.committed_ena()
                    .copied()
                    .unwrap_or(gateway_ereport_messages::Ena::NONE),
                &mut tx_buf[..],
            ) {
                Ok(size) => size,
                Err(e) => {
                    // Packrat's mad. Reject the request.
                    //
                    // Presently, the only time we'd see an error here is if we
                    // have yet to generate a restart ID.
                    count!(Event::ReadError(e));
                    continue;
                }
            },
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
