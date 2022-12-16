// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Support for recording kernel crashes/failures such that they can be found by
//! tooling.
//!
//! This module defines the following binary interface to debuggers:
//!
//! - `kern::fail::KERNEL_HAS_FAILED` is a `bool`. It is cleared to zero (false) before
//!   entry to kernel main, and set to one (true) if the kernel reaches the
//!   `die` function (either explicitly or as a result of a `panic!`). If it
//!   contains any other value, the kernel has either not yet booted, or has
//!   corrupted memory on the way down.
//!
//! - `kern::fail::KERNEL_EPITAPH` is an array of `u8` -- assume its size is
//!   configurable. The `die` routine writes as much of the failure reason into
//!   this buffer (as UTF-8) as possible, truncating if the buffer fills. The
//!   number of bytes written isn't recorded anywhere; instead, for printing,
//!   trim off any trailing NUL bytes.

use core::fmt::{Display, Write};
use core::sync::atomic::Ordering;

/// Flag that gets set to `true` by all failure reporting functions, giving
/// tools a one-stop-shop for doing kernel triage.
#[used]
static mut KERNEL_HAS_FAILED: bool = false;

const EPITAPH_LEN: usize = 128;

/// The "epitaph" buffer records up to `EPITAPH_LEN` bytes of description of the
/// event that caused the kernel to fail, padded with NULs.
#[used]
static mut KERNEL_EPITAPH: [u8; EPITAPH_LEN] = [0; EPITAPH_LEN];

fn begin_epitaph() -> &'static mut [u8; EPITAPH_LEN] {
    // We'd love to use an AtomicBool here but we gotta support ARMv6M.
    let previous_fail =
        core::mem::replace(unsafe { &mut KERNEL_HAS_FAILED }, true);
    if previous_fail {
        // Welp, you've called begin_epitaph twice, suggesting a recursive
        // panic. We can't very well panic in response to this since it'll just
        // make the problem worse.
        loop {
            // Platform-independent NOP
            core::sync::atomic::fence(Ordering::SeqCst);
        }
    }

    // Safety: we can get a mutable reference to the epitaph because only one
    // execution of this function will successfully set that flag.
    unsafe { &mut KERNEL_EPITAPH }
}

#[inline(always)]
pub fn die(msg: impl Display) -> ! {
    die_impl(&msg)
}

#[inline(never)]
fn die_impl(msg: &dyn Display) -> ! {
    let buf = begin_epitaph();
    let mut writer = Eulogist { dest: buf };
    write!(writer, "{}", msg).ok();

    loop {
        // Platform-independent NOP
        core::sync::atomic::fence(Ordering::SeqCst);
    }
}

struct Eulogist {
    dest: &'static mut [u8],
}

impl Write for Eulogist {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let s = s.as_bytes();
        let n = s.len().min(self.dest.len());
        let (dest, leftovers) = {
            let taken = core::mem::replace(&mut self.dest, &mut []);
            taken.split_at_mut(n)
        };
        dest.copy_from_slice(&s[..n]);
        self.dest = leftovers;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    die(info)
}
