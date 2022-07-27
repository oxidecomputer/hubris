// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::*;

task_slot!(NET, net);

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::echo;

    loop {
        // Tiiiiiny payload buffer
        let mut rx_data_buf = [0u8; 64];
        match net.recv_packet(SOCKET, &mut rx_data_buf) {
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
                        Err(NetError::QueueFull) => {
                            // Our outgoing queue is full; wait for space.
                            sys_recv_closed(&mut [], 1, TaskId::KERNEL)
                                .unwrap();
                        }
                        Err(NetError::NotYours) => panic!(),
                        Err(NetError::InvalidVLan) => panic!(),
                        Err(NetError::Other) => panic!(),
                        // `send_packet()` can't return QueueEmpty or
                        // InvalidPort
                        Err(NetError::QueueEmpty) => unreachable!(),
                        Err(NetError::InvalidPort) => unreachable!(),
                    }
                }
            }
            Err(NetError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_closed(&mut [], 1, TaskId::KERNEL).unwrap();
            }
            Err(NetError::NotYours) => panic!(),
            Err(NetError::InvalidVLan) => panic!(),
            Err(NetError::Other) => panic!(),
            // `recv_packet()` can't return QueueFull or InvalidPort
            Err(NetError::QueueFull) => unreachable!(),
            Err(NetError::InvalidPort) => unreachable!(),
        }

        // Try again.
    }
}

static UDP_ECHO_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
