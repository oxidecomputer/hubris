// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Received IPC messages, and the obligation to reply to them.

use core::mem::ManuallyDrop;

use userlib::{RecvMessage, ReplyFaultReason, TaskId};

/// An IPC message received by [`Executor::recv`](crate::Executor::recv),
/// which hasn't been replied to yet.
///
/// The sender stays blocked until it gets a reply, so a `Message` carries the
/// obligation to send one. Replying consumes it. To defer the reply, hold on
/// to the `Message` (e.g. hand it to an async task) and reply later.
///
/// Dropping a `Message` without replying panics. Losing track of a reply is a
/// bug in this task, and panicking restarts it, which unblocks the sender
/// with a "server restarted" error, rather than leaving it blocked forever.
#[must_use = "the sender stays blocked until this message is replied to"]
pub struct Message {
    inner: RecvMessage,
}

impl Message {
    pub(crate) fn new(inner: RecvMessage) -> Self {
        Self { inner }
    }

    /// The task that sent this message.
    pub fn sender(&self) -> TaskId {
        self.inner.sender
    }

    /// The operation code the sender passed.
    pub fn operation(&self) -> u32 {
        self.inner.operation
    }

    /// The number of bytes of the message received into the buffer passed to
    /// `recv`.
    ///
    /// The kernel reports the length *copied*, not the length sent, so a
    /// message too long for the buffer looks like one that exactly fills it.
    /// To detect over-long messages, receive into a buffer one byte longer
    /// than the longest message you accept, and reject any that fill it.
    pub fn message_len(&self) -> usize {
        self.inner.message_len
    }

    /// The longest reply the sender will accept. Replying with more than this
    /// faults *this* task.
    pub fn response_capacity(&self) -> usize {
        self.inner.response_capacity
    }

    /// The number of leases the sender attached.
    pub fn lease_count(&self) -> usize {
        self.inner.lease_count
    }

    /// Replies to the sender, unblocking it.
    #[allow(clippy::disallowed_methods)]
    pub fn reply(self, code: u32, data: &[u8]) {
        let this = ManuallyDrop::new(self);
        userlib::sys_reply(this.inner.sender, code, data);
    }

    /// Faults the sender instead of replying, e.g. because it sent a
    /// malformed message.
    #[allow(clippy::disallowed_methods)]
    pub fn reply_fault(self, reason: ReplyFaultReason) {
        let this = ManuallyDrop::new(self);
        userlib::sys_reply_fault(this.inner.sender, reason);
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        panic!("hubris-async: Message dropped without a reply");
    }
}
