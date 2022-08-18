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

    const SOCKET: SocketName = SocketName::rpc;

    loop {
        // Tiiiiiny payload buffer
        let mut rx_data_buf = [0u8; 256];
        let mut tx_data_buf = [0u8; 256];
        match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_data_buf,
        ) {
            Ok(mut meta) => {
                let task =
                    u16::from_be_bytes(rx_data_buf[0..2].try_into().unwrap());
                let op =
                    u16::from_be_bytes(rx_data_buf[2..4].try_into().unwrap());
                let nreply =
                    u16::from_be_bytes(rx_data_buf[4..6].try_into().unwrap());
                let nbytes =
                    u16::from_be_bytes(rx_data_buf[6..8].try_into().unwrap());

                let (code, _) = sys_send(
                    TaskId(task),
                    op,
                    &rx_data_buf[8..(nbytes as usize + 8)],
                    &mut tx_data_buf[4..(nreply as usize + 4)],
                    &[],
                );
                tx_data_buf[0..4].copy_from_slice(&code.to_be_bytes());
                meta.size = nreply as u32 + 4;

                net.send_packet(
                    SOCKET,
                    meta,
                    &tx_data_buf[0..meta.size as usize],
                )
                .unwrap();
            }
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_closed(&mut [], 1, TaskId::KERNEL).unwrap();
            }
            Err(RecvError::NotYours | RecvError::Other) => panic!(),
        }
        // Try again.
    }
}
