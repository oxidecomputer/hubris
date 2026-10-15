// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ordinary synchronous client that exercises `task-async-demo` or
//! `task-blocking-demo`, whichever its `server` task slot points at. The two
//! servers behave identically, so this client works the same against both.
//!
//! Forever: sends a burst of messages to the echo server and checks that each
//! comes back unchanged, posts the `poke` notification, then goes quiet for
//! long enough that the echo server's idle timeout fires.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ringbuf::counted_ringbuf;
use userlib::{hl, sys_post, sys_send, task_slot};

task_slot!(SERVER, server);

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
#[cfg_attr(not(target_os = "none"), derive(Debug))]
enum Trace {
    #[count(skip)]
    None,
    /// An echo came back matching what we sent.
    EchoOk {
        seq: u32,
        len: usize,
    },
    /// The server replied with a non-zero code.
    EchoError {
        seq: u32,
        rc: u32,
    },
    /// The server replied with something other than what we sent.
    EchoMismatch {
        seq: u32,
        len: usize,
    },
    /// We sent a deliberately oversized message and it was rejected.
    TooLongRejected {
        rc: u32,
    },
    Poke,
}

counted_ringbuf!(Trace, 32, Trace::None);

/// Records a trace event in the ringbuf, and on the host also prints it to
/// stderr (stdout carries a host task's syscalls).
macro_rules! trace {
    ($event:expr) => {{
        let event = $event;
        #[cfg(not(target_os = "none"))]
        eprintln!("[demo_client] {event:?}");
        ringbuf::ringbuf_entry!(event);
    }};
}

const OP_ECHO: u16 = 1;
const BURST: u32 = 3;
const BURST_GAP: u64 = 200;
/// Longer than the echo server's 5s idle timeout.
const QUIET: u64 = 8_000;

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let mut seq = 0u32;
    loop {
        for _ in 0..BURST {
            seq = seq.wrapping_add(1);
            echo(seq);
            hl::sleep_for(BURST_GAP);
        }

        // One message longer than the server's 32-byte limit, which it should
        // reject rather than truncate.
        let big = [0xAAu8; 40];
        let mut resp = [0u8; 40];
        let (rc, _) =
            sys_send(SERVER.get_task_id(), OP_ECHO, &big, &mut resp, &[]);
        trace!(Trace::TooLongRejected { rc });

        trace!(Trace::Poke);
        sys_post(SERVER.get_task_id(), SERVER_POKE_MASK);

        hl::sleep_for(QUIET);
    }
}

fn echo(seq: u32) {
    // A message whose contents vary with `seq`, so a stale or mixed-up reply
    // would be caught.
    let mut msg = [0u8; 12];
    msg[..4].copy_from_slice(&seq.to_le_bytes());
    msg[4..].copy_from_slice(b"hubris!!");
    let mut resp = [0u8; 16];

    let (rc, len) =
        sys_send(SERVER.get_task_id(), OP_ECHO, &msg, &mut resp, &[]);
    if rc != 0 {
        trace!(Trace::EchoError { seq, rc });
    } else if resp[..len] != msg {
        trace!(Trace::EchoMismatch { seq, len });
    } else {
        trace!(Trace::EchoOk { seq, len });
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/server.rs"));
