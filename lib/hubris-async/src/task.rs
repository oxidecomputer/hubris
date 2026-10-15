// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Static storage for async tasks.

use core::cell::{Cell, UnsafeCell};
use core::future::Future;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Maximum alignment supported for a task's future.
const MAX_ALIGN: usize = 8;

type PollFn = unsafe fn(&'static TaskHeader, &mut Context<'_>) -> Poll<()>;

/// Type-erased bookkeeping for a single async task.
///
/// Every spawned task's header is linked into the executor's intrusive task
/// list through `next`.
pub(crate) struct TaskHeader {
    /// Set once the task has been spawned; never cleared.
    spawned: Cell<bool>,
    /// Polls the task's future. `None` if the task isn't spawned or has
    /// already completed.
    pub(crate) poll_fn: Cell<Option<PollFn>>,
    /// Pointer to the future's storage.
    future: Cell<*mut ()>,
    /// Whether the task should be polled on the next executor pass.
    pub(crate) ready: Cell<bool>,
    /// The earliest time this task has asked to be woken at, if any.
    pub(crate) wake_at: Cell<Option<u64>>,
    /// Next task in the executor's task list.
    pub(crate) next: Cell<Option<&'static TaskHeader>>,
}

impl TaskHeader {
    const fn new() -> Self {
        Self {
            spawned: Cell::new(false),
            poll_fn: Cell::new(None),
            future: Cell::new(core::ptr::null_mut()),
            ready: Cell::new(false),
            wake_at: Cell::new(None),
            next: Cell::new(None),
        }
    }

    /// Requests that the task be woken no later than `deadline`.
    pub(crate) fn schedule_wake(&self, deadline: u64) {
        match self.wake_at.get() {
            Some(cur) if cur <= deadline => {}
            _ => self.wake_at.set(Some(deadline)),
        }
    }
}

#[repr(C, align(8))]
struct FutureBuf<const N: usize>([MaybeUninit<u8>; N]);

/// Statically-allocated storage for one async task whose future fits in `N`
/// bytes.
///
/// You normally don't use this directly; the [`task!`](crate::task) macro
/// declares one sized for your `async fn`.
pub struct TaskStorage<const N: usize> {
    header: TaskHeader,
    future: UnsafeCell<FutureBuf<N>>,
}

// Safety: Hubris tasks are single-threaded with no user-level interrupt
// handlers, so a `TaskStorage` can never actually be accessed concurrently.
unsafe impl<const N: usize> Sync for TaskStorage<N> {}

impl<const N: usize> TaskStorage<N> {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            header: TaskHeader::new(),
            future: UnsafeCell::new(FutureBuf([MaybeUninit::uninit(); N])),
        }
    }

    /// Moves `future` into this storage and returns a token that can be
    /// passed to [`Executor::spawn`](crate::Executor::spawn).
    ///
    /// # Panics
    ///
    /// If this storage has already been used to spawn a task.
    pub fn spawn<F>(&'static self, future: F) -> SpawnToken
    where
        F: Future<Output = ()> + 'static,
    {
        const {
            assert!(core::mem::size_of::<F>() <= N);
            assert!(core::mem::align_of::<F>() <= MAX_ALIGN);
        }

        if self.header.spawned.replace(true) {
            panic!("async task spawned twice");
        }

        let ptr = self.future.get().cast::<F>();
        // Safety: the buffer is large and aligned enough for `F` (checked
        // above), and the `spawned` flag guarantees nothing else has a
        // reference into it.
        unsafe { ptr.write(future) };

        self.header.future.set(ptr.cast());
        self.header.poll_fn.set(Some(poll_task::<F>));
        self.header.ready.set(true);

        SpawnToken {
            header: &self.header,
        }
    }
}

/// Polls a task's future, dropping it in place once it completes.
///
/// # Safety
///
/// `header.future` must point to a live, pinned `F`.
unsafe fn poll_task<F: Future<Output = ()>>(
    header: &'static TaskHeader,
    cx: &mut Context<'_>,
) -> Poll<()> {
    let fut = header.future.get().cast::<F>();
    // Safety: the future lives in a `static`, so it never moves.
    let r = unsafe { Pin::new_unchecked(&mut *fut) }.poll(cx);
    if r.is_ready() {
        // Safety: the executor never polls this task again once it has
        // returned `Ready`.
        unsafe { core::ptr::drop_in_place(fut) };
    }
    r
}

/// A spawned-but-not-yet-running async task, to be handed to
/// [`Executor::spawn`](crate::Executor::spawn).
#[must_use = "async tasks do nothing unless passed to `Executor::spawn`"]
pub struct SpawnToken {
    pub(crate) header: &'static TaskHeader,
}

/// Helper trait used by [`task!`](crate::task) to name the future type
/// returned by an `async fn` with arguments `Args`.
pub trait TaskFn<Args> {
    type Fut: Future<Output = ()>;
}

macro_rules! impl_task_fn {
    ($($arg:ident),*) => {
        impl<F, Fut, $($arg),*> TaskFn<($($arg,)*)> for F
        where
            F: Fn($($arg),*) -> Fut,
            Fut: Future<Output = ()>,
        {
            type Fut = Fut;
        }
    };
}

impl_task_fn!();
impl_task_fn!(A);
impl_task_fn!(A, B);
impl_task_fn!(A, B, C);
impl_task_fn!(A, B, C, D);

/// Returns the size of the future produced by `f`. Used by
/// [`task!`](crate::task) to size a task's static storage.
#[doc(hidden)]
pub const fn __future_size<F: TaskFn<Args>, Args>(_f: &F) -> usize {
    core::mem::size_of::<F::Fut>()
}
