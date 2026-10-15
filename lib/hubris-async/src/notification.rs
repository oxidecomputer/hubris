// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Waiting for notifications.
//!
//! The executor only includes a notification bit in its receive mask while
//! some async task is waiting on it, so bits nobody is waiting for stay
//! pending in the kernel, as they would in a synchronous task. Once received,
//! bits are kept on the async task that asked for them until a [`wait`]
//! future on that task consumes them, so they survive the future being
//! dropped (e.g. by losing a `select`).
//!
//! If several async tasks wait on the same bit, each of them sees it.
//!
//! The executor's own timer notification is reserved; waiting on it panics.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::executor::task_from_waker;

/// Waits until at least one of the notifications in `mask` arrives. Resolves
/// to the bits from `mask` that arrived.
pub fn wait(mask: u32) -> Notification {
    Notification { mask }
}

/// Future returned by [`wait`].
#[must_use = "futures do nothing unless awaited"]
pub struct Notification {
    mask: u32,
}

impl Future for Notification {
    type Output = u32;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
        let task = task_from_waker(cx.waker());
        let pending = task.notif_pending.get();
        let got = pending & self.mask;
        if got != 0 {
            task.notif_pending.set(pending & !got);
            Poll::Ready(got)
        } else {
            task.notif_mask.set(task.notif_mask.get() | self.mask);
            Poll::Pending
        }
    }
}
