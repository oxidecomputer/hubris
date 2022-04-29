// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hubris kernel.
//!
//! This is the application-independent portion of the operating system, and the
//! main part that runs in privileged mode.
//!
//! This code outside of the `arch` module is *intended* to be portable to at
//! least ARMv7-M and RV32I, but it is only being actively developed and tested
//! on ARMv7-M, so it's entirely possible that some ARM-isms have
//! unintentionally leaked into the portable parts.
//!
//! # Design principles
//!
//! While this isn't a *deeply* principled kernel, there are some basic ideas
//! that appear consistently.
//!
//! 1. Separate compilation. Allow the kernel, and each task of the application,
//!    to be compiled separately and then combined.
//! 2. Static configuration. As much as possible, the system should take a
//!    single shape specified at compile time.
//! 3. A strong preference for safe code where reasonable.
//! 4. A preference for simple and clear algorithms over fast and clever
//!    algorithms. (This also relates to the preference for safe code, since
//!    most clever algorithms used in kernels wind up requiring `unsafe`.)

#![cfg_attr(target_os = "none", no_std)]
#![feature(asm)]
#![feature(naked_functions)]

#[macro_use]
pub mod arch;

pub mod app;
pub mod err;
pub mod kipc;
pub mod profiling;
pub mod startup;
pub mod syscalls;
pub mod task;
pub mod time;
pub mod umem;
