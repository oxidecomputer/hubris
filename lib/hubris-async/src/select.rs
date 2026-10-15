// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A minimal two-way `select`.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Result of [`select`]: which future finished first, and its output.
pub enum Either<A, B> {
    First(A),
    Second(B),
}

/// Runs two futures concurrently and resolves with the output of whichever
/// finishes first, dropping the other. If both are ready on the same poll,
/// `a` wins.
///
/// For example, a receive with a timeout:
///
/// ```ignore
/// match select(ipc::recv(&mut buf), Timer::after(1000)).await {
///     Either::First(msg) => { /* got a message */ }
///     Either::Second(()) => { /* timed out */ }
/// }
/// ```
pub fn select<A: Future, B: Future>(a: A, b: B) -> Select<A, B> {
    Select { a, b }
}

/// Future returned by [`select`].
#[must_use = "futures do nothing unless awaited"]
pub struct Select<A, B> {
    a: A,
    b: B,
}

impl<A: Future, B: Future> Future for Select<A, B> {
    type Output = Either<A::Output, B::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: we never move `a` or `b` out of `self`, so they stay pinned
        // along with it.
        let this = unsafe { self.get_unchecked_mut() };
        let a = unsafe { Pin::new_unchecked(&mut this.a) };
        if let Poll::Ready(x) = a.poll(cx) {
            return Poll::Ready(Either::First(x));
        }
        let b = unsafe { Pin::new_unchecked(&mut this.b) };
        if let Poll::Ready(x) = b.poll(cx) {
            return Poll::Ready(Either::Second(x));
        }
        Poll::Pending
    }
}
