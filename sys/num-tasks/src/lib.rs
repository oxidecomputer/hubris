// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Allows applications to depend, at compile time, on the number of tasks in
//! the image.
//!
//! `num_tasks::NUM_TASKS` is a `const` `usize` giving the total task count.
//! This can be used to size tables, which in turn lets tasks effectively "add a
//! field" to all tasks in the system, outside the kernel.

#![no_std]
#![forbid(clippy::wildcard_imports)]

include!(concat!(env!("OUT_DIR"), "/tasks.rs"));
