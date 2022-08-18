// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::*;

task_slot!(NET, net);

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum RpcReply {
    Ok,
    BadImageId,
    NBytesMismatch,
    NBytesOverflow,
    NReplyMismatch,
    NReplyOverflow,
}

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::rpc;
    let image_id = kipc::read_image_id();

    // We expect request packets to be tightly packed in the order
    //      image id: u64,
    //      task: u16,
    //      op: u16,
    //      nreply: u16,
    //      nbytes: u16,
    //      data: nbytes
    //
    // `humility rpc` must cooperate with this layout!
    // We use the image id to make sure that we're compatible.
    //
    // The output format is dependent on status code.  The first byte is always
    // a member of `RpcReply` as a `u8`.
    // - `NBytesMismatch`, `NBytesOverflow`, `NReplyMismatch`, `NReplyOverflow`
    //   return nothing else (so the reply is 1 byte)
    // - `BadImageId` is followed by the *actual* 64-bit image id as a big-endian
    //   value
    // - `Ok` is followed by the return code as a 32-bit, big-endian value,
    //   then by `nreply` bytes of reply.
    loop {
        let mut rx_data_buf = [0u8; 1024];
        let mut tx_data_buf = [0u8; 1024];
        match net.recv_packet(
            SOCKET,
            LargePayloadBehavior::Discard,
            &mut rx_data_buf,
        ) {
            Ok(mut meta) => {
                // Hard-coded to match behavior in `humility rpc`
                let expected_id =
                    u64::from_be_bytes(rx_data_buf[0..8].try_into().unwrap());
                let task =
                    u16::from_be_bytes(rx_data_buf[8..10].try_into().unwrap());
                let op =
                    u16::from_be_bytes(rx_data_buf[10..12].try_into().unwrap());
                let nreply =
                    u16::from_be_bytes(rx_data_buf[12..14].try_into().unwrap())
                        as usize;
                let nbytes =
                    u16::from_be_bytes(rx_data_buf[14..16].try_into().unwrap())
                        as usize;

                if expected_id != image_id {
                    tx_data_buf[0] = RpcReply::BadImageId as u8;
                    rx_data_buf[1..9].copy_from_slice(&image_id.to_be_bytes());
                    meta.size = 9;
                } else if meta.size != 16 + nbytes as u32 {
                    tx_data_buf[0] = RpcReply::NBytesMismatch as u8;
                    meta.size = 1;
                } else if nbytes + 16 > rx_data_buf.len() {
                    tx_data_buf[0] = RpcReply::NBytesOverflow as u8;
                    meta.size = 1;
                } else if nreply + 4 > tx_data_buf.len() {
                    tx_data_buf[0] = RpcReply::NReplyOverflow as u8;
                    meta.size = 1;
                } else {
                    let (rc, len) = sys_send(
                        TaskId(task),
                        op,
                        &rx_data_buf[16..(nbytes + 16)],
                        &mut tx_data_buf[5..(nreply + 4)],
                        &[],
                    );
                    if len != nreply {
                        tx_data_buf[0] = RpcReply::NReplyMismatch as u8;
                        meta.size = 1;
                    } else {
                        tx_data_buf[0] = RpcReply::Ok as u8;
                        tx_data_buf[1..5].copy_from_slice(&rc.to_be_bytes());
                        meta.size = nreply as u32 + 5;
                    }
                }

                net.send_packet(
                    SOCKET,
                    meta,
                    &tx_data_buf[0..(meta.size as usize)],
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
