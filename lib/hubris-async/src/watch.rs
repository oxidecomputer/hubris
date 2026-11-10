// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sharing a value between async tasks.
//!
//! A [`Watch`] holds the latest value published by one async task, and lets
//! any number of others read it or wait for the next one. It's meant for
//! fan-out, like one task sampling some hardware state and the rest reacting
//! to it, without each of them doing its own syscalls.
//!
//! This is modeled on `tokio::sync::watch` (and `embassy_sync::watch`), but
//! is simpler: there are no separate sender and receiver handles, and so no
//! per-receiver record of which values have been seen. See
//! [`Watch::changed`] for what that means in practice.

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

/// A value published by one async task and watched by others.
///
/// `N` is the most async tasks that can wait on the watch at once. Several
/// futures in the same async task share a slot.
pub struct Watch<T, const N: usize> {
    value: Cell<T>,
    /// Bumped by every [`publish`](Watch::publish), so a waiter can tell
    /// whether there's been one since it started waiting.
    generation: Cell<u32>,
    /// The async tasks to wake on the next publish. Entries are cleared by
    /// every publish, so one left behind by a dropped future costs at most a
    /// spurious wake.
    waiters: [Cell<Option<Waker>>; N],
}

// Safety: Hubris tasks are single-threaded, so a `Watch` can't be accessed
// concurrently; see the crate docs.
unsafe impl<T, const N: usize> Sync for Watch<T, N> {}

impl<T: Copy, const N: usize> Watch<T, N> {
    /// Creates a watch holding `value`.
    pub const fn new(value: T) -> Self {
        Self {
            value: Cell::new(value),
            generation: Cell::new(0),
            waiters: [const { Cell::new(None) }; N],
        }
    }

    /// Returns the latest value.
    pub fn get(&self) -> T {
        self.value.get()
    }

    /// Replaces the value, and wakes every async task waiting on the watch,
    /// even if the value is unchanged.
    pub fn publish(&self, value: T) {
        self.value.set(value);
        self.generation.set(self.generation.get().wrapping_add(1));
        for w in &self.waiters {
            if let Some(waker) = w.take() {
                waker.wake();
            }
        }
    }

    /// Waits for the next [`publish`](Watch::publish) after this call, and
    /// resolves to the latest value.
    ///
    /// Only publishes after this call count. Unlike a `tokio::sync::watch`
    /// receiver, a watch doesn't remember what each waiter has already seen,
    /// so a value published *before* the call, e.g. while the caller was
    /// awaiting something else, won't complete it. To act on the current
    /// value as well as future ones, check [`get`](Watch::get) first and
    /// then call this with no `.await` in between, or use
    /// [`wait_for`](Watch::wait_for), which does exactly that:
    ///
    /// ```ignore
    /// loop {
    ///     if is_interesting(&WATCH.get()) {
    ///         break;
    ///     }
    ///     WATCH.changed().await;
    /// }
    /// ```
    ///
    /// Since Hubris tasks are single-threaded, nothing can publish between
    /// the `get` and the `changed`, so no publish is missed.
    pub fn changed(&self) -> Changed<'_, T, N> {
        Changed {
            watch: self,
            seen: self.generation.get(),
        }
    }

    /// Resolves to the first value, starting with the current one, for which
    /// `f` returns `true`.
    ///
    /// Like `tokio::sync::watch::Receiver::wait_for`, this checks the
    /// current value before waiting, so it returns immediately if it already
    /// matches. After that it checks each published value; one that matches
    /// but is replaced before this future is polled again isn't seen.
    pub async fn wait_for(&self, mut f: impl FnMut(&T) -> bool) -> T {
        loop {
            let value = self.get();
            if f(&value) {
                return value;
            }
            self.changed().await;
        }
    }

    /// Arranges for `waker` to be woken by the next publish.
    ///
    /// # Panics
    ///
    /// If more than `N` async tasks are already waiting.
    fn register(&self, waker: &Waker) {
        let mut free = None;
        for w in &self.waiters {
            match w.take() {
                Some(old) if old.will_wake(waker) => {
                    w.set(Some(old));
                    return;
                }
                Some(old) => w.set(Some(old)),
                None => free = free.or(Some(w)),
            }
        }
        match free {
            Some(w) => w.set(Some(waker.clone())),
            None => panic!("too many async tasks waiting on a Watch"),
        }
    }
}

/// Future returned by [`Watch::changed`].
#[must_use = "futures do nothing unless awaited"]
pub struct Changed<'a, T, const N: usize> {
    watch: &'a Watch<T, N>,
    seen: u32,
}

impl<T: Copy, const N: usize> Future for Changed<'_, T, N> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if self.watch.generation.get() != self.seen {
            Poll::Ready(self.watch.get())
        } else {
            self.watch.register(cx.waker());
            Poll::Pending
        }
    }
}
