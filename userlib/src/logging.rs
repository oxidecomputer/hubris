//! User logging
//!
//! This module provides helper functionality related to logging. If your task
//! wants to produce log messages, this is how it's done.
//!
//! This module is enabled by a feature flag. Please include `userlib` in your
//! task like this:
//!
//! ```toml
//! userlib = {path = "../userlib", features = ["log"]}
//! ```
//!
//! Enabling this feature will add 258 bytes of flash to your task. This is
//! because we need to create a small buffer to produce the full log message in
//! before sending it to the log task.
//!
//! There are two macros you can use, currently: `debug!` and `error!`. We may
//! add more if needed in the future.
//!
//! # Examples
//!
//! Basic usage:
//!
//! ```
//! userlib::debug!("Ping task starting!");
//! ```
//!
//! # Internals: defmt
//!
//! These macros are built off of the defmt crate, and basically re-export them.
//! We do this for a few reasons. First of all, by defining our own macros, we
//! insert an architectural seam; if we ever desire switching away from defmt,
//! we'll be able to do so.
//!
//! Beyond that, doing this means that you don't have to go through the trouble
//! of depending on defmt in your task; you can use this version from userlib,
//! which you are probably already using. Given the whole system shares one
//! version of userlib, this also has a side benefit of not accidentally using
//! different defmt versions in different tasks. Defmt already has the ability
//! to detect such a situation and show an error, but ideally, you could never
//! get into that inconsistent state in the first place.

use crate::{sys_send, Lease};

use abi::{Generation, TaskId};

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

#[macro_export]
macro_rules! debug {
    ($fmt:expr) => (defmt::debug!($fmt));
    ($fmt:expr, $($arg:tt)*) => (defmt::debug!($fmt, $($arg)*));
}

#[macro_export]
macro_rules! error {
    ($fmt:expr) => (defmt::error!($fmt));
    ($fmt:expr, $($arg:tt)*) => (defmt::error!($fmt, $($arg)*));
}

#[defmt::global_logger]
struct Logger;

static mut BUFFER: [u8; 256] = [0; 256];
static mut BUFFER_LEN: usize = 0;

static ACQUIRED: AtomicBool = AtomicBool::new(false);

impl defmt::Write for Logger {
    fn write(&mut self, bytes: &[u8]) {
        unsafe {
            let last_pos = bytes.len() + BUFFER_LEN;
            BUFFER[BUFFER_LEN..last_pos].copy_from_slice(bytes);
            BUFFER_LEN += bytes.len();
        }
    }
}

unsafe impl defmt::Logger for Logger {
    fn acquire() -> Option<NonNull<dyn defmt::Write>> {
        if ACQUIRED.swap(true, Ordering::SeqCst) {
            panic!();
        }

        Some(NonNull::from(&Logger as &dyn defmt::Write))
    }

    unsafe fn release(_: NonNull<dyn defmt::Write>) {
        ACQUIRED.store(false, Ordering::SeqCst);

        send_to_log_task();

        BUFFER_LEN = 0;
    }
}

// We want to send stuff to the log task, so if we're not in standalone
// mode, let's do that.
#[cfg(not(standalone))]
fn send_to_log_task() {
    extern "C" {
        static __log_task_id: u16;
    }

    unsafe {
        let log_id = *(&__log_task_id as *const u16);

        let log =
            TaskId::for_index_and_gen(log_id as usize, Generation::default());
        sys_send(log, 1, &[], &mut [], &[Lease::from(&BUFFER[0..BUFFER_LEN])]);
    }
}

// If we are standalone, we don't know what the log task is, so let's not
// try and do that.
#[cfg(standalone)]
fn send_to_log_task() {}
