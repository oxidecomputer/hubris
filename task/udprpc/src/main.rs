// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

task_slot!(NET, net);

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum RpcReply {
    Ok,
    /// The RPC packet was too short to include the complete header
    TooShort,
    /// The RPC packet's image ID does not match ours
    BadImageId,
    /// The size of the packet does not agree with the number of bytes specified
    /// in the `nbytes` field of the packet
    NBytesMismatch,
    /// The `nbytes` field of the packet imply that the packet overflows the
    /// `rx_data_buf`
    NBytesOverflow,
    /// The result of the function does not agree with the number of bytes
    /// specified in the `nreply` field of the packet
    NReplyMismatch,
    /// The output would overflow `tx_data_buf`
    NReplyOverflow,
}

/// Header for an RPC request
///
/// `humility` must cooperate with this layout, which is mirrored in `doppel.rs`
#[derive(Copy, Clone, Debug, FromBytes)]
#[repr(C)]
struct RpcHeader {
    image_id: u64,
    task: u16,
    op: u16,
    nreply: u16,
    nbytes: u16,
}

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::rpc;
    let image_id = kipc::read_image_id();

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
                // We can always read the header, because it's raw data.  It may
                // not be valid, though, if the packet is too short (checked
                // in the conditional below).
                const HEADER_SIZE: usize = core::mem::size_of::<RpcHeader>();
                let header =
                    RpcHeader::read_from(&rx_data_buf[..HEADER_SIZE]).unwrap();

                const REPLY_PREFIX_SIZE: usize = 5;

                // We deliberately assign to `r` here then manipulate it;
                // otherwise, the compiler won't include RpcReply in DWARF data.
                let r = if (meta.size as usize) < HEADER_SIZE {
                    RpcReply::TooShort
                } else if image_id != header.image_id {
                    tx_data_buf[1..9].copy_from_slice(image_id.as_bytes());
                    RpcReply::BadImageId
                } else if meta.size as usize
                    != HEADER_SIZE + header.nbytes as usize
                {
                    RpcReply::NBytesMismatch
                } else if header.nbytes as usize + HEADER_SIZE
                    > rx_data_buf.len()
                {
                    RpcReply::NBytesOverflow
                } else if header.nreply as usize + 4 > tx_data_buf.len() {
                    RpcReply::NReplyOverflow
                } else {
                    let rx_data =
                        &rx_data_buf[HEADER_SIZE..][..header.nbytes as usize];
                    let tx_data = &mut tx_data_buf[REPLY_PREFIX_SIZE..]
                        [..header.nreply as usize];
                    let (rc, len) = sys_send(
                        TaskId(header.task),
                        header.op,
                        rx_data,
                        tx_data,
                        &[],
                    );
                    if rc == 0 && len != header.nreply as usize {
                        RpcReply::NReplyMismatch
                    } else {
                        tx_data_buf[1..5].copy_from_slice(&rc.to_be_bytes());
                        RpcReply::Ok
                    }
                };

                tx_data_buf[0] = r as u8;
                meta.size = match r {
                    RpcReply::TooShort
                    | RpcReply::NBytesMismatch
                    | RpcReply::NReplyOverflow
                    | RpcReply::NBytesOverflow
                    | RpcReply::NReplyMismatch => 1,
                    RpcReply::BadImageId => 9,
                    RpcReply::Ok => header.nreply as u32 + 5,
                };

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
