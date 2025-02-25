// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::{sys_recv_notification, task_slot};

task_slot!(NET, net);

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::echo;

    loop {
        // Tiiiiiny payload buffer
        let mut rx_data_buf = [0u8; 64];
        match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_data_buf,
        ) {
            Ok(meta) => {
                // A packet! We want to turn it right around. Deserialize the
                // packet header; unwrap because we trust the server.
                UDP_ECHO_COUNT
                    .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                // Now we know how many bytes to return.
                let tx_bytes = &rx_data_buf[..meta.size as usize];

                loop {
                    match net.send_packet(SOCKET, meta, tx_bytes) {
                        Ok(()) => break,
                        Err(SendError::QueueFull) => {
                            // Our outgoing queue is full; wait for space.
                            sys_recv_notification(notifications::SOCKET_MASK);
                        }
                        Err(SendError::ServerRestarted) => {
                            // Welp, lost an echo, we'll just soldier on.
                        }
                    }
                }
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_notification(notifications::SOCKET_MASK);
            }
            Err(RecvError::ServerRestarted) => {
                // `net` restarted, the poor thing; just retry.
            }
        }

        // Try again.
    }
}

static UDP_ECHO_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
