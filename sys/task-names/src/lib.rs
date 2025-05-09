// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Allows compile-time retrieval of task names in the current image.
//!
//! The code is generated, but here's what you can expect:
//!
//! `TASK_NAMES` is a `static` array of `&str`.
//!
//! `MAX_TASK_NAME` is a `const` `usize` giving the number of bytes in the
//! longest task name. This can be useful for sizing buffers.

#![no_std]
#![forbid(clippy::wildcard_imports)]

include!(concat!(env!("OUT_DIR"), "/tasks.rs"));
