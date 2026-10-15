// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Waiting for time to pass.
//!
//! All times are in Hubris kernel ticks, which are milliseconds on all
//! currently supported platforms.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use userlib::sys_get_timer;

use crate::executor::task_from_waker;

/// Returns the current kernel time, in ticks.
pub fn now() -> u64 {
    sys_get_timer().now
}

/// A future that completes once the kernel time reaches a deadline.
#[must_use = "timers do nothing unless awaited"]
pub struct Timer {
    deadline: u64,
}

impl Timer {
    /// Completes once the kernel time is at least `deadline`.
    pub fn at(deadline: u64) -> Self {
        Self { deadline }
    }

    /// Completes `ticks` ticks from now.
    pub fn after(ticks: u64) -> Self {
        Self::at(now().saturating_add(ticks))
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if now() >= self.deadline {
            Poll::Ready(())
        } else {
            task_from_waker(cx.waker()).schedule_wake(self.deadline);
            Poll::Pending
        }
    }
}

/// Fires at a fixed period without accumulating drift.
///
/// If a tick is missed (e.g. because some other task hogged the executor),
/// subsequent calls to [`Ticker::next`] complete immediately until it has
/// caught up.
pub struct Ticker {
    next: u64,
    period: u64,
}

impl Ticker {
    /// Creates a ticker whose first tick is `period` ticks from now.
    pub fn every(period: u64) -> Self {
        Self {
            next: now().saturating_add(period),
            period,
        }
    }

    /// Waits for the next tick.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Timer {
        let t = Timer::at(self.next);
        self.next = self.next.saturating_add(self.period);
        t
    }
}
