// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Receiving IPC messages as a server.
//!
//! A Hubris task has a single inbox, so all async tasks share it. The
//! executor only does an open receive while some async task is awaiting
//! [`recv`], and receives into its own buffer (sized by the executor's
//! `RECV_LEN` parameter). The message then waits there until a [`recv`]
//! future copies it out.
//!
//! Holding the message in the executor, rather than receiving straight into
//! the caller's buffer, means a message is never lost when a `recv` future is
//! dropped after the message arrived but before it was polled (e.g. by a
//! `select` whose other branch also completed). Losing it would leave the
//! sender blocked forever. Instead it goes to the next `recv`, and no further
//! messages are received until it's been taken.
//!
//! Replying is done with the ordinary, non-blocking `userlib::sys_reply`.

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use userlib::RecvMessage;

use crate::executor::task_from_waker;

/// The task-wide inbox: the executor's receive buffer, and the message in it
/// (if any) that no `Recv` has taken yet.
pub(crate) struct Inbox {
    buf: Cell<*const u8>,
    buf_len: Cell<usize>,
    pending: Cell<Option<RecvMessage>>,
}

// Safety: Hubris tasks are single-threaded with no user-level interrupt
// handlers, so the inbox can never actually be accessed concurrently.
unsafe impl Sync for Inbox {}

pub(crate) static INBOX: Inbox = Inbox {
    buf: Cell::new(core::ptr::null()),
    buf_len: Cell::new(0),
    pending: Cell::new(None),
};

impl Inbox {
    /// Points the inbox at the executor's receive buffer, which must outlive
    /// all uses of the inbox.
    pub(crate) fn set_buffer(&self, buf: *const u8, len: usize) {
        self.buf.set(buf);
        self.buf_len.set(len);
    }

    pub(crate) fn is_full(&self) -> bool {
        // `RecvMessage` isn't `Copy`, so peek by taking and putting back.
        let m = self.pending.take();
        let full = m.is_some();
        self.pending.set(m);
        full
    }

    pub(crate) fn put(&self, msg: RecvMessage) {
        self.pending.set(Some(msg));
    }

    /// Takes the pending message, if any, copying its body into `out`.
    fn take(&self, out: &mut [u8]) -> Option<RecvMessage> {
        let msg = self.pending.take()?;
        let n = msg.message_len.min(self.buf_len.get()).min(out.len());
        // Safety: `buf` points at the executor's `buf_len`-byte receive
        // buffer, which the executor doesn't touch while tasks are polled.
        unsafe {
            core::ptr::copy_nonoverlapping(self.buf.get(), out.as_mut_ptr(), n)
        };
        Some(msg)
    }
}

/// Waits for an IPC message to arrive, and copies its body into `buf`.
///
/// The returned `message_len` is the length the sender sent. If it's longer
/// than `buf` or the executor's `RECV_LEN`, the body was truncated to the
/// shorter of the two, just as with `sys_recv_open`.
///
/// Every message received **must** be replied to (or explicitly deferred),
/// or its sender stays blocked.
pub fn recv(buf: &mut [u8]) -> Recv<'_> {
    Recv { buf }
}

/// Future returned by [`recv`].
#[must_use = "futures do nothing unless awaited"]
pub struct Recv<'a> {
    buf: &'a mut [u8],
}

impl Future for Recv<'_> {
    type Output = RecvMessage;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<RecvMessage> {
        match INBOX.take(self.buf) {
            Some(msg) => Poll::Ready(msg),
            None => {
                task_from_waker(cx.waker()).wants_msg.set(true);
                Poll::Pending
            }
        }
    }
}
