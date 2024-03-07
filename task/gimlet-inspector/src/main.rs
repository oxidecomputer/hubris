// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Gimlet Inspector provides a deliberately limited set of IPCs over the
//! network, for extracting diagnostic data from a live system. This is intended
//! to supplement the more general `dump_agent`.

#![no_std]
#![no_main]

use counters::*;
use drv_gimlet_seq_api::{SeqError, Sequencer};
use gimlet_inspector_protocol::{
    QueryV0, Request, SequencerRegistersResponseV0, ANY_RESPONSE_V0_MAX_SIZE,
    REQUEST_TRAILER,
};
use hubpack::SerializedSize;
use task_net_api::*;
use userlib::*;

task_slot!(NET, net);
task_slot!(SEQ, seq);

#[derive(Count, Copy, Clone)]
enum Event {
    RecvPacket,
    RequestRejected,
    Respond,
}

counters!(Event);

#[export_name = "main"]
fn main() -> ! {
    // Look up our peer task IDs and make clients.
    let net = Net::from(NET.get_task_id());
    let seq = Sequencer::from(SEQ.get_task_id());

    const SOCKET: SocketName = SocketName::inspector;

    loop {
        // These buffers are currently kept kinda small because our protocol
        // messages are small.
        let mut rx_data_buf = [0u8; Request::MAX_SIZE + REQUEST_TRAILER];
        let mut tx_data_buf = [0u8; ANY_RESPONSE_V0_MAX_SIZE];

        match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_data_buf,
        ) {
            Ok(mut meta) => {
                count!(Event::RecvPacket);

                let Ok((request, _trailer)) = hubpack::deserialize::<Request>(&rx_data_buf) else {
                    // We ignore malformatted, truncated, etc. packets.
                    count!(Event::RequestRejected);
                    continue;
                };

                match request {
                    Request::V0(QueryV0::SequencerRegisters) => {
                        let result = seq.read_fpga_regs();
                        let (resp, trailer) = match result {
                            Ok(regs) => (
                                SequencerRegistersResponseV0::Success,
                                Some(regs),
                            ),
                            Err(SeqError::ServerRestarted) => (
                                SequencerRegistersResponseV0::SequencerTaskDead,
                                None,
                            ),
                            Err(_) => {
                                // The SeqError type represents a mashing
                                // together of all possible errors for all
                                // possible sequencer IPC operations. The only
                                // one we _expect_ here is ReadRegsFailed.
                                (SequencerRegistersResponseV0::SequencerReadRegsFailed, None)
                            }
                        };
                        let mut len =
                            hubpack::serialize(&mut tx_data_buf, &resp)
                                .unwrap_lite();
                        if let Some(t) = trailer {
                            tx_data_buf[len..len + t.len()].copy_from_slice(&t);
                            len += t.len();
                        }
                        meta.size = len as u32;
                    }
                }

                // With the response packet prepared, we may need to attempt
                // sending more than once.
                loop {
                    match net.send_packet(
                        SOCKET,
                        meta,
                        &tx_data_buf[0..(meta.size as usize)],
                    ) {
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
                            sys_recv_closed(
                                &mut [],
                                notifications::SOCKET_MASK,
                                TaskId::KERNEL,
                            )
                            .unwrap_lite();
                        }
                    }
                }
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_closed(
                    &mut [],
                    notifications::SOCKET_MASK,
                    TaskId::KERNEL,
                )
                .unwrap_lite();
            }
            Err(RecvError::ServerRestarted) => {
                // `net` restarted (probably due to the watchdog); just retry.
            }
            Err(RecvError::NotYours | RecvError::Other) => panic!(),
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
