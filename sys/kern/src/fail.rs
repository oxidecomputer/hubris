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

#[cfg(not(feature = "nano"))]
use core::{
    fmt::{Display, Write},
    sync::atomic::Ordering,
};

/// Flag that gets set to `true` by all failure reporting functions, giving
/// tools a one-stop-shop for doing kernel triage.
#[used]
static mut KERNEL_HAS_FAILED: bool = false;

#[cfg(not(feature = "nano"))]
const EPITAPH_LEN: usize = 128;

/// The "epitaph" buffer records up to `EPITAPH_LEN` bytes of description of the
/// event that caused the kernel to fail, padded with NULs.
#[cfg(not(feature = "nano"))]
#[used]
static mut KERNEL_EPITAPH: [u8; EPITAPH_LEN] = [0; EPITAPH_LEN];

#[cfg(not(feature = "nano"))]
fn begin_epitaph() -> &'static mut [u8; EPITAPH_LEN] {
    // We'd love to use an AtomicBool here but we gotta support ARMv6M.
    // This could probably become SyncUnsafeCell in a future where it exists.
    //
    // Safety: we only access this function from this one site, and only zero or
    // one times in practice -- and never from a context where concurrency or
    // interrupts are enabled.
    let previous_fail = unsafe {
        core::ptr::replace(core::ptr::addr_of_mut!(KERNEL_HAS_FAILED), true)
    };
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
    unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_EPITAPH) }
}

#[cfg(not(feature = "nano"))]
#[inline(always)]
pub fn die(msg: impl Display) -> ! {
    die_impl(&msg)
}

#[cfg(not(feature = "nano"))]
#[inline(never)]
fn die_impl(msg: &dyn Display) -> ! {
    let buf = begin_epitaph();
    let mut writer = Eulogist { dest: buf };
    write!(writer, "{msg}").ok();

    loop {
        // Platform-independent NOP
        core::sync::atomic::fence(Ordering::SeqCst);
    }
}

#[cfg(not(feature = "nano"))]
struct Eulogist {
    dest: &'static mut [u8],
}

#[cfg(not(feature = "nano"))]
impl Write for Eulogist {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let s = s.as_bytes();
        let n = s.len().min(self.dest.len());
        let (dest, leftovers) = {
            let taken = core::mem::take(&mut self.dest);
            taken.split_at_mut(n)
        };
        dest.copy_from_slice(&s[..n]);
        self.dest = leftovers;
        Ok(())
    }
}

#[cfg(not(feature = "nano"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    die(info)
}

#[cfg(feature = "nano")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    unsafe {
        KERNEL_HAS_FAILED = true;
    }
    loop {
        cortex_m::asm::nop();
    }
}
