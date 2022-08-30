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

    let smasher = claim_static_smasher();

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
                            sys_recv_closed(&mut [], 1, TaskId::KERNEL)
                                .unwrap();
                        }
                        Err(SendError::NotYours) => panic!(),
                        Err(SendError::InvalidVLan) => panic!(),
                        Err(SendError::Other) => panic!(),
                    }
                }
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_closed(&mut [], 1, TaskId::KERNEL).unwrap();
            }
            Err(RecvError::NotYours) => panic!(),
            Err(RecvError::Other) => panic!(),
        }

        // Try again.
    }
}

const STACK_SMASHER_BUF_SIZE: usize = 1024;

struct StackSmasher {
    counter1: Option<usize>,
    counter2: usize,
    data: [u8; STACK_SMASHER_BUF_SIZE],
}

impl Default for StackSmasher {
    fn default() -> Self {
        Self {
            counter1: None,
            counter2: 0,
            data: [0; STACK_SMASHER_BUF_SIZE],
        }
    }
}

fn claim_static_smasher() -> &'static mut StackSmasher {
    let array = mutable_statics::mutable_statics! {
        static mut BUF: [StackSmasher; 1] = [Default::default(); _];
    };
    &mut array[0]
}

static UDP_ECHO_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
