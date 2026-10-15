// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The executor run loop and its waker.

use core::cell::{Cell, UnsafeCell};
use core::task::{Context, RawWaker, RawWakerVTable, Waker};

use userlib::{
    TaskId, sys_get_timer, sys_recv_notification, sys_recv_open, sys_set_timer,
};

use crate::ipc::INBOX;
use crate::task::{SpawnToken, TaskHeader};

/// A single-threaded executor that runs statically-allocated async tasks
/// inside one Hubris task.
///
/// `RECV_LEN` is the size of the executor's IPC receive buffer: the longest
/// message that [`ipc::recv`](crate::ipc::recv) can deliver without
/// truncation. Leave it at 0 if no async task acts as a server.
pub struct Executor<const RECV_LEN: usize = 0> {
    /// Head of the intrusive list of spawned tasks.
    tasks: Cell<Option<&'static TaskHeader>>,
    /// The notification bit(s) to use for this task's timer.
    timer_mask: u32,
    /// Incoming IPC messages land here, and wait until an `ipc::recv` future
    /// copies them out.
    recv_buf: UnsafeCell<[u8; RECV_LEN]>,
}

/// Everything the spawned tasks are currently waiting on, collected just
/// before the executor goes to sleep.
struct Interest {
    /// Earliest pending timer deadline.
    deadline: Option<u64>,
    /// Union of all notification bits being waited on.
    notif_mask: u32,
    /// Whether any task is waiting for an IPC message.
    wants_msg: bool,
}

impl<const RECV_LEN: usize> Executor<RECV_LEN> {
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
            recv_buf: UnsafeCell::new([0; RECV_LEN]),
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
    /// ready tasks are polled. When no task is ready, the executor blocks in
    /// a single `sys_recv` that covers everything the tasks are waiting on:
    ///
    /// - the task timer is set for the earliest deadline,
    /// - the notification mask is the timer bit plus every bit any task is
    ///   waiting on, and
    /// - if any task is waiting for a message (and none is already queued),
    ///   the receive is open; otherwise it only accepts notifications.
    pub fn run(&self) -> ! {
        // This never returns, so `recv_buf` outlives every `Recv` future.
        INBOX.set_buffer(self.recv_buf.get().cast(), RECV_LEN);

        loop {
            let now = sys_get_timer().now;
            let interest = self.scan(now);

            if self.poll_ready() {
                // Polling may have readied other tasks or changed what they're
                // waiting on, so take another lap before sleeping.
                continue;
            }

            // Nothing was polled, so `interest` is still accurate.
            self.sleep(interest);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &'static TaskHeader> {
        core::iter::successors(self.tasks.get(), |t| t.next.get())
    }

    /// Marks every task whose deadline is at or before `now` as ready, and
    /// collects what the remaining tasks are waiting on.
    fn scan(&self, now: u64) -> Interest {
        let mut interest = Interest {
            deadline: None,
            notif_mask: 0,
            wants_msg: false,
        };
        for task in self.iter() {
            if let Some(at) = task.wake_at.get() {
                if at <= now {
                    task.wake_at.set(None);
                    task.ready.set(true);
                } else {
                    interest.deadline =
                        Some(interest.deadline.map_or(at, |n| n.min(at)));
                }
            }
            interest.notif_mask |= task.notif_mask.get();
            interest.wants_msg |= task.wants_msg.get();
        }
        interest
    }

    /// Blocks until something the tasks are waiting on happens, and wakes
    /// the tasks that were waiting on it.
    fn sleep(&self, interest: Interest) {
        if interest.notif_mask & self.timer_mask != 0 {
            panic!("async task waited on the executor's timer notification");
        }

        // If `deadline` is `None`, the timer is disabled. If additionally no
        // task wants a message or notification, every task is finished or
        // waiting on something that can never happen, and we sleep forever.
        sys_set_timer(interest.deadline, self.timer_mask);
        let mask = interest.notif_mask | self.timer_mask;

        // Only open our inbox if someone is waiting to take a message out of
        // it, and the previous message has been taken. Otherwise senders
        // stay queued in the kernel until we're ready for them.
        let bits = if interest.wants_msg && !INBOX.is_full() {
            // Safety: the buffer is only otherwise touched by `Recv::poll`,
            // which can't run while we're blocked here.
            let buf = unsafe { &mut *self.recv_buf.get() };
            let msg = sys_recv_open(buf, mask);
            if msg.sender == TaskId::KERNEL {
                msg.operation
            } else {
                INBOX.put(msg);
                for task in self.iter().filter(|t| t.wants_msg.get()) {
                    task.ready.set(true);
                }
                0
            }
        } else {
            sys_recv_notification(mask).get_raw_bits()
        };

        // Hand notification bits to whoever was waiting on them. Timer bits
        // aren't anyone's; timers are checked against the clock in `scan`.
        for task in self.iter() {
            let got = bits & task.notif_mask.get();
            if got != 0 {
                task.notif_pending.set(task.notif_pending.get() | got);
                task.ready.set(true);
            }
        }
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
