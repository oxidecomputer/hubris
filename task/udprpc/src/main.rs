// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::*;
use zerocopy::{FromBytes, IntoBytes, LittleEndian, U16, U64};

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
    /// The output would overflow `tx_data_buf`
    NReplyOverflow,
}

/// Header for an RPC request
///
/// `humility` must cooperate with this layout, which is mirrored in `doppel.rs`
#[derive(Copy, Clone, Debug, FromBytes)]
#[repr(C)]
struct RpcHeader {
    image_id: U64<LittleEndian>,
    task: U16<LittleEndian>,
    op: U16<LittleEndian>,
    nreply: U16<LittleEndian>,
    nbytes: U16<LittleEndian>,
}

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::rpc;

    // We use the image id to make sure that we're compatible, since we're
    // sending raw bytes using `sys_send`.  This isn't robust against malicious
    // behavior, but prevents basic user error.
    let image_id = kipc::read_image_id();

    // The output format is dependent on status code.  The first byte is always
    // a member of `RpcReply` as a `u8`.
    // - `NBytesMismatch`, `NReplyOverflow` return nothing else (so the reply is
    //   1 byte)
    // - `BadImageId` is followed by the *actual* 64-bit image id as a
    //   little-endian value
    // - `Ok` is followed by the return code as a 32-bit, little-endian value,
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
                const HEADER_SIZE: usize = core::mem::size_of::<RpcHeader>();
                const REPLY_PREFIX_SIZE: usize = 5;

                // We deliberately assign to `r` here then manipulate it;
                // otherwise, the compiler won't include `RpcReply` in DWARF
                // data.
                let (r, nreply) = if (meta.size as usize) < HEADER_SIZE {
                    (RpcReply::TooShort, 0)
                } else {
                    // We can always read the header, since it's raw data
                    let header =
                        RpcHeader::read_from(&rx_data_buf[..HEADER_SIZE])
                            .unwrap_lite();

                    let nbytes = header.nbytes.get() as usize;
                    let mut nreply = header.nreply.get() as usize;

                    let r = if image_id != header.image_id.get() {
                        tx_data_buf[1..9].copy_from_slice(image_id.as_bytes());
                        RpcReply::BadImageId
                    } else if meta.size as usize != HEADER_SIZE + nbytes {
                        RpcReply::NBytesMismatch
                    } else if nreply + REPLY_PREFIX_SIZE > tx_data_buf.len() {
                        RpcReply::NReplyOverflow
                    } else {
                        // This is the happy path: unpack the data and execute
                        // the sys_send which actually calls the target.
                        let rx_data = &rx_data_buf[HEADER_SIZE..][..nbytes];

                        // The returned data is stored after the reply prefix,
                        // which consists of a one-byte `RpcReply` then a
                        // u32 return code from the `sys_send` call.
                        let tx_data =
                            &mut tx_data_buf[REPLY_PREFIX_SIZE..][..nreply];

                        let task_id =
                            sys_refresh_task_id(TaskId(header.task.get()));
                        let (rc, len) = sys_send(
                            task_id,
                            header.op.get(),
                            rx_data,
                            tx_data,
                            &[],
                        );

                        // Store the return code
                        tx_data_buf[1..5].copy_from_slice(&rc.to_be_bytes());

                        // For idol calls with ssmarshal or hubpack encoding,
                        // the actual reply len may be less than `nreply` (the
                        // max possible encoding length); fill in the
                        // possibly-truncated length here. We know `len` is at
                        // most `nreply`: if it weren't, `sys_send()` would have
                        // faulted us for providing a too-short buffer.
                        nreply = len;

                        RpcReply::Ok
                    };
                    (r, nreply)
                };

                // Store the `RpcReply` return code and return size
                tx_data_buf[0] = r as u8;
                meta.size = match r {
                    RpcReply::TooShort
                    | RpcReply::NBytesMismatch
                    | RpcReply::NReplyOverflow => 1,
                    RpcReply::BadImageId => {
                        (1 + core::mem::size_of_val(&image_id)) as u32
                    }
                    RpcReply::Ok => (nreply + REPLY_PREFIX_SIZE) as u32,
                };

                loop {
                    match net.send_packet(
                        SOCKET,
                        meta,
                        &tx_data_buf[0..(meta.size as usize)],
                    ) {
                        Ok(()) => break,
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
            Err(RecvError::QueueEmpty) => {
                // Our incoming queue is empty. Wait for more packets.
                sys_recv_notification(notifications::SOCKET_MASK);
            }
            Err(RecvError::ServerRestarted) => {
                // `net` restarted (probably due to the watchdog); just retry.
            }
        }
        // Try again.
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
