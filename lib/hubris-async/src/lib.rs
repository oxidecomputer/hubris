// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A minimal, single-task async executor for Hubris.
//!
//! This is a proof-of-concept executor that lets a single Hubris task run
//! several cooperatively-scheduled futures ("async tasks"). It requires no
//! kernel support beyond the ordinary task timer and notifications.
//!
//! Async tasks can wait for:
//!
//! - time to pass, via [`time::Timer`] and [`time::Ticker`], and
//! - notifications, via [`notification::wait`].
//!
//! These compose freely, e.g. with [`select`]. Each of these leaf futures
//! records what it's waiting for on its async task when polled. When no task
//! is ready to run, [`Executor::step`] sets the task timer for the earliest
//! deadline and returns a notification mask covering every bit anyone is
//! waiting on. The caller then blocks in `sys_recv` with that mask, and
//! hands any notifications back to the executor to route to the async tasks
//! waiting on them.
//!
//! IPC messages are *not* received by async tasks. The task's own receive
//! loop handles them synchronously, typically via [`Executor::recv`], which
//! does all of the above around a `sys_recv_open`:
//!
//! ```ignore
//! loop {
//!     if let Some(msg) = executor.recv(&mut buf) {
//!         // handle the message...
//!         msg.reply(0, &response);
//!     }
//! }
//! ```
//!
//! Async tasks are statically allocated, in the style of `embassy-executor`:
//!
//! ```ignore
//! hubris_async::task! {
//!     async fn blinky(period: u64) {
//!         let mut ticker = Ticker::every(period);
//!         loop {
//!             ticker.next().await;
//!             // ...
//!         }
//!     }
//! }
//!
//! let executor = Executor::new();
//! executor.spawn(blinky(500));
//! executor.run()
//! ```
//!
//! # Declaring an async task in the app.toml
//!
//! A task using this crate must set `async = true` in its app.toml entry,
//! and have a notification named `timer`, which the executor uses for its
//! timer:
//!
//! ```toml
//! [tasks.my_task]
//! name = "task-my-task"
//! async = true
//! notifications = ["timer"]
//! ```
//!
//! The build fails if either is missing.
//!
//! # Syscalls
//!
//! Some syscalls interfere with the executor: receiving behind its back
//! steals messages or notification bits that async tasks are waiting for,
//! setting the timer clobbers the executor's deadline, sleeping stalls every
//! async task, and a raw `sys_reply` sidesteps [`Message`]'s reply tracking.
//!
//! [`sys`] re-exports the parts of `userlib` that are safe to use from an
//! async task; prefer it to using `userlib` directly. [`danger`] re-exports
//! the rest, for the rare case you really need one.
//!
//! This is enforced by `cargo xtask clippy`: for tasks with `async = true`,
//! it denies calls to the dangerous syscalls (whichever path they're named
//! by) in the task *and in every workspace crate it depends on*. A deliberate
//! use needs an explicit `#[allow(clippy::disallowed_methods)]`, which also
//! makes it easy to find. The list lives in this crate's `lint/clippy.toml`.
//!
//! # Concurrency
//!
//! Hubris tasks are single-threaded and have no user-level interrupt handlers,
//! so nothing in here can ever be accessed concurrently. This is what lets us
//! use plain `Cell`s inside `static`s.

#![no_std]

mod config {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
}
mod executor;
mod message;
pub mod notification;
mod select;
mod task;
pub mod time;

pub use executor::Executor;
pub use message::Message;
pub use select::{Either, Select, select};
pub use task::{__future_size, SpawnToken, TaskFn, TaskStorage};

/// The parts of `userlib` that are safe to use from a task running the
/// executor.
///
/// Blocking calls here (like `sys_send`) stall every async task until they
/// return, but don't otherwise interfere with the executor.
pub mod sys {
    pub use userlib::{
        BorrowInfo, FromPrimitive, IrqStatus, Lease, LeaseAttributes,
        NotificationBits, ReplyFaultReason, TaskId, TimerState, ToPrimitive,
        UnwrapLite, kipc, sys_borrow_info, sys_borrow_read, sys_borrow_write,
        sys_get_timer, sys_irq_control, sys_irq_control_clear_pending,
        sys_irq_status, sys_panic, sys_post, sys_refresh_task_id, sys_send,
        task_slot, units,
    };
}

/// The parts of `userlib` that interfere with the executor.
///
/// Calls to these are denied by `cargo xtask clippy` in async tasks; see the
/// [crate docs](crate#syscalls). They're re-exported here so that a
/// deliberate use stands out, but each call still needs
/// `#[allow(clippy::disallowed_methods)]`, with a comment explaining why
/// it's safe.
pub mod danger {
    pub use userlib::hl::{sleep_for, sleep_until};
    pub use userlib::{
        RecvMessage, set_timer_relative, sys_recv, sys_recv_closed,
        sys_recv_notification, sys_recv_open, sys_reply, sys_reply_fault,
        sys_set_timer,
    };
}

/// Declares a statically-allocated async task.
///
/// This turns an `async fn` into a plain `fn` with the same arguments that
/// returns a [`SpawnToken`], which can be handed to [`Executor::spawn`]. The
/// future's storage is a `static` sized exactly to fit the future, so no
/// allocator is required.
///
/// Each task can only be spawned once; calling the function a second time
/// panics.
///
/// Limitations of this proof-of-concept: arguments must be plain identifiers
/// (no patterns), the function may not be generic, and the task must return
/// `()`.
#[macro_export]
macro_rules! task {
    (
        $(#[$meta:meta])*
        $vis:vis async fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) $body:block
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $ty),*) -> $crate::SpawnToken {
            async fn __task($($arg: $ty),*) $body

            static STORAGE: $crate::TaskStorage<
                { $crate::__future_size(&__task) },
            > = $crate::TaskStorage::new();

            STORAGE.spawn(__task($($arg),*))
        }
    };
}
