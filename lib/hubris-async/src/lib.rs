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
//!         // handle the message and reply...
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
//! let executor = Executor::new(notifications::TIMER_MASK);
//! executor.spawn(blinky(500));
//! executor.run()
//! ```
//!
//! # Concurrency
//!
//! Hubris tasks are single-threaded and have no user-level interrupt handlers,
//! so nothing in here can ever be accessed concurrently. This is what lets us
//! use plain `Cell`s inside `static`s.

#![no_std]

mod executor;
pub mod notification;
mod select;
mod task;
pub mod time;

pub use executor::Executor;
pub use select::{Either, Select, select};
pub use task::{__future_size, SpawnToken, TaskFn, TaskStorage};

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
