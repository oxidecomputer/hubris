// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The executor run loop and its waker.

use core::cell::Cell;
use core::task::{Context, RawWaker, RawWakerVTable, Waker};

use userlib::{sys_get_timer, sys_recv_notification, sys_set_timer};

use crate::task::{SpawnToken, TaskHeader};

/// A single-threaded executor that runs statically-allocated async tasks
/// inside one Hubris task.
pub struct Executor {
    /// Head of the intrusive list of spawned tasks.
    tasks: Cell<Option<&'static TaskHeader>>,
    /// The notification bit(s) to use for this task's timer.
    timer_mask: u32,
}

impl Executor {
    /// Creates an executor that will use `timer_mask` as the notification
    /// for the Hubris task timer.
    ///
    /// The executor takes over the task timer entirely; nothing else in the
    /// task should call `sys_set_timer`.
    pub const fn new(timer_mask: u32) -> Self {
        Self {
            tasks: Cell::new(None),
            timer_mask,
        }
    }

    /// Adds a task to the executor. It is first polled once [`run`] starts.
    ///
    /// [`run`]: Executor::run
    pub fn spawn(&self, token: SpawnToken) {
        let header = token.header;
        header.next.set(self.tasks.get());
        self.tasks.set(Some(header));
    }

    /// Runs all spawned tasks forever.
    ///
    /// Each pass, tasks whose deadlines have passed are marked ready and all
    /// ready tasks are polled. When no task is ready, the Hubris task timer is
    /// set for the earliest pending deadline and the task blocks until it
    /// fires.
    pub fn run(&self) -> ! {
        loop {
            let now = sys_get_timer().now;
            let next_deadline = self.expire_timers(now);

            if self.poll_ready() {
                // Polling may have readied other tasks or registered new
                // deadlines, so take another lap before sleeping.
                continue;
            }

            // Nothing was polled, so `next_deadline` is still accurate. If
            // it's `None`, every task is either finished or waiting on
            // something that can never happen, and we sleep forever.
            sys_set_timer(next_deadline, self.timer_mask);
            sys_recv_notification(self.timer_mask);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &'static TaskHeader> {
        core::iter::successors(self.tasks.get(), |t| t.next.get())
    }

    /// Marks every task whose deadline is at or before `now` as ready, and
    /// returns the earliest deadline still pending.
    fn expire_timers(&self, now: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        for task in self.iter() {
            if let Some(at) = task.wake_at.get() {
                if at <= now {
                    task.wake_at.set(None);
                    task.ready.set(true);
                } else {
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
            }
        }
        next
    }

    /// Polls every ready task once. Returns whether any task was polled.
    fn poll_ready(&self) -> bool {
        let mut polled = false;
        for task in self.iter() {
            if !task.ready.replace(false) {
                continue;
            }
            let Some(poll) = task.poll_fn.get() else {
                continue;
            };
            polled = true;

            let waker = waker_for(task);
            let mut cx = Context::from_waker(&waker);
            // Safety: `poll_fn` is only `Some` while the future is live.
            if unsafe { poll(task, &mut cx) }.is_ready() {
                task.poll_fn.set(None);
                task.wake_at.set(None);
            }
        }
        polled
    }
}

static VTABLE: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake, waker_drop);

fn waker_for(task: &'static TaskHeader) -> Waker {
    let raw = RawWaker::new((task as *const TaskHeader).cast(), &VTABLE);
    // Safety: our vtable functions uphold the `RawWaker` contract; the data
    // pointer is a `&'static TaskHeader` so cloning and dropping are no-ops.
    unsafe { Waker::from_raw(raw) }
}

unsafe fn waker_clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &VTABLE)
}

unsafe fn waker_wake(data: *const ()) {
    // Safety: `data` always comes from `waker_for`.
    let task = unsafe { &*data.cast::<TaskHeader>() };
    task.ready.set(true);
}

unsafe fn waker_drop(_data: *const ()) {}

/// Returns the task that owns `waker`.
///
/// # Panics
///
/// If `waker` didn't come from this executor.
pub(crate) fn task_from_waker(waker: &Waker) -> &'static TaskHeader {
    if !core::ptr::eq(waker.vtable(), &VTABLE) {
        panic!("hubris-async timer polled outside of hubris-async executor");
    }
    // Safety: the vtable matches, so the data pointer came from `waker_for`.
    unsafe { &*waker.data().cast::<TaskHeader>() }
}
