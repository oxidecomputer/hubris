//! Hubris kernel model.
//!
//! This code is intended to lay out the design concepts for the Hubris kernel
//! implementation and make some points about algorithm implementation. It may
//! evolve to become the actual kernel, or it may not.
//!
//! Currently, this is intended to be portable to both ARM and x86, for testing
//! and simulation purposes.
//!
//! # Algorithm Naivety Principles
//!
//! This implementation uses *really naive algorithms*. This is deliberate. The
//! intent is:
//!
//! 1. To use safe Rust for as much as possible.
//! 2. To use easily understood and debugged algorithms.
//! 3. To revisit these decisions if they become performance problems.
//!
//! Assumptions enabling our naivete:
//!
//! - The total number of tasks is fixed (in a given build) and small. Say, less
//!   than 200.
//! - We are not attempting to achieve predictably low worst-case execution
//!   bounds or any realtime nonsense like that.

#![cfg_attr(target_os = "none", no_std)]
#![feature(asm)]
#![feature(naked_functions)]

pub mod app;
pub mod arch;
pub mod startup;
pub mod syscalls;
pub mod task;
pub mod time;
pub mod umem;

use crate::task::FaultInfo;

#[derive(Copy, Clone, Debug)]
pub struct InteractFault {
    pub sender: Option<FaultInfo>,
    pub recipient: Option<FaultInfo>,
}

impl InteractFault {
    fn in_sender(fi: impl Into<FaultInfo>) -> Self {
        Self {
            sender: Some(fi.into()),
            recipient: None,
        }
    }

    fn in_recipient(fi: impl Into<FaultInfo>) -> Self {
        Self {
            sender: None,
            recipient: Some(fi.into()),
        }
    }
}
