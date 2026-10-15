// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The executor run loop and its waker.

use core::cell::Cell;
use core::task::{Context, RawWaker, RawWakerVTable, Waker};

use userlib::{
    RecvMessage, TaskId, sys_get_timer, sys_recv_notification, sys_recv_open,
    sys_set_timer,
};

use crate::task::{SpawnToken, TaskHeader};

/// A single-threaded executor that runs statically-allocated async tasks
/// inside one Hubris task.
///
/// The executor doesn't own the task's receive loop. Instead, it's driven
/// from one of:
///
/// - [`Executor::run`], for tasks that don't serve IPC,
/// - [`Executor::recv`], for a hand-written server loop that receives
///   messages synchronously while async tasks run in the background, or
/// - [`Executor::step`] and [`Executor::handle_notification`], for plugging
///   into some other receive loop, such as `idol_runtime::dispatch`.
pub struct Executor {
    /// Head of the intrusive list of spawned tasks.
    tasks: Cell<Option<&'static TaskHeader>>,
    /// The notification bit(s) to use for this task's timer.
    timer_mask: u32,
    /// The notification mask computed by the last `step`.
    mask: Cell<u32>,
}

impl Executor {
    /// Creates an executor that will use `timer_mask` as the notification
    /// for the Hubris task timer.
    ///
    /// The executor takes over the task timer entirely; nothing else in the
    /// task should call `sys_set_timer`, and no async task may wait on
    /// `timer_mask` as a notification.
    pub const fn new(timer_mask: u32) -> Self {
        Self {
            tasks: Cell::new(None),
            timer_mask,
            mask: Cell::new(timer_mask),
        }
    }

    /// Adds a task to the executor. It is first polled by the next
    /// [`step`](Executor::step).
    pub fn spawn(&self, token: SpawnToken) {
        let header = token.header;
        header.next.set(self.tasks.get());
        self.tasks.set(Some(header));
    }

    /// Runs async tasks until none of them can make progress, then gets
    /// ready to block.
    ///
    /// This sets the task timer for the earliest deadline any async task is
    /// waiting on, and returns the notification mask to pass to `sys_recv`:
    /// the timer bit, plus every bit any async task is waiting on. Pass
    /// whatever notification bits `sys_recv` returns to
    /// [`handle_notification`](Executor::handle_notification).
    ///
    /// Call this before every receive. If code outside the executor could
    /// have woken an async task since the last call, the receive might
    /// otherwise block with that task still waiting to run.
    pub fn step(&self) -> u32 {
        let deadline = loop {
            let now = sys_get_timer().now;
            let (deadline, notif_mask) = self.scan(now);
            if !self.poll_ready() {
                // Nothing was polled, so what we scanned is still accurate.
                if notif_mask & self.timer_mask != 0 {
                    panic!(
                        "async task waited on the executor's timer \
                         notification"
                    );
                }
                self.mask.set(notif_mask | self.timer_mask);
                break deadline;
            }
            // Polling may have readied other tasks or changed what they're
            // waiting on, so take another lap.
        };

        // If `deadline` is `None`, the timer is disabled.
        sys_set_timer(deadline, self.timer_mask);
        self.mask.get()
    }

    /// The notification mask returned by the most recent
    /// [`step`](Executor::step).
    ///
    /// This is handy for implementing `NotificationHandler` for use with
    /// `idol_runtime::dispatch`, whose `current_notification_mask` is called
    /// after `step`.
    pub fn notification_mask(&self) -> u32 {
        self.mask.get()
    }

    /// Hands notification bits returned by `sys_recv` to the async tasks
    /// waiting on them, and marks those tasks ready.
    ///
    /// Timer bits are ignored; timers are checked against the clock in
    /// `step`. Bits nobody is waiting on are dropped.
    pub fn handle_notification(&self, bits: u32) {
        for task in self.iter() {
            let got = bits & task.notif_mask.get();
            if got != 0 {
                task.notif_pending.set(task.notif_pending.get() | got);
                task.ready.set(true);
            }
        }
    }

    /// Runs async tasks until they're all waiting, then blocks until either
    /// an IPC message arrives or something they're waiting on happens.
    ///
    /// Returns `Some` if a message was received into `buf`, which the caller
    /// must reply to, as with `sys_recv_open`. Returns `None` if we were
    /// woken by a timer or notification, which has already been routed to
    /// the async tasks; just call `recv` again.
    ///
    /// ```ignore
    /// loop {
    ///     if let Some(msg) = executor.recv(&mut buf) {
    ///         // handle the message and reply...
    ///     }
    /// }
    /// ```
    pub fn recv(&self, buf: &mut [u8]) -> Option<RecvMessage> {
        let mask = self.step();
        let msg = sys_recv_open(buf, mask);
        if msg.sender == TaskId::KERNEL {
            self.handle_notification(msg.operation);
            None
        } else {
            Some(msg)
        }
    }

    /// Runs async tasks forever, for tasks that don't serve IPC.
    pub fn run(&self) -> ! {
        loop {
            let mask = self.step();
            let bits = sys_recv_notification(mask).get_raw_bits();
            self.handle_notification(bits);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &'static TaskHeader> {
        core::iter::successors(self.tasks.get(), |t| t.next.get())
    }

    /// Marks every task whose deadline is at or before `now` as ready, and
    /// returns the earliest deadline still pending, and the union of all
    /// notification bits being waited on.
    fn scan(&self, now: u64) -> (Option<u64>, u32) {
        let mut deadline: Option<u64> = None;
        let mut notif_mask = 0;
        for task in self.iter() {
            if let Some(at) = task.wake_at.get() {
                if at <= now {
                    task.wake_at.set(None);
                    task.ready.set(true);
                } else {
                    deadline = Some(deadline.map_or(at, |n| n.min(at)));
                }
            }
            notif_mask |= task.notif_mask.get();
        }
        (deadline, notif_mask)
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

            // The task re-registers whatever it's still waiting on as it's
            // polled.
            task.clear_interest();

            let waker = waker_for(task);
            let mut cx = Context::from_waker(&waker);
            // Safety: `poll_fn` is only `Some` while the future is live.
            if unsafe { poll(task, &mut cx) }.is_ready() {
                task.poll_fn.set(None);
                task.clear_interest();
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
