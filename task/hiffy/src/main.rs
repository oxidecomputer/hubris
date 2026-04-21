// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HIF interpreter
//!
//! HIF is the Hubris/Humility Interchange Format, a simple stack-based
//! machine that allows for some dynamic programmability of Hubris.  In
//! particular, this task provides a HIF interpreter to allow for Humility
//! commands like `humility i2c`, `humility pmbus` and `humility jefe`.  The
//! debugger places HIF in [`HIFFY_TEXT`], and then indicates that text is
//! present by incrementing [`HIFFY_KICK`].  This task executes the specified
//! HIF, with the return stack located in [`HIFFY_RSTACK`].

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use hif::*;
use static_cell::*;
use userlib::*;

mod common;

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        pub mod stm32h7;
        use crate::stm32h7::*;
    } else if #[cfg(feature = "lpc55")] {
        pub mod lpc55;
        use crate::lpc55::*;
    } else if #[cfg(feature = "stm32g0")] {
        pub mod stm32g0;
        use crate::stm32g0::*;
    } else if #[cfg(feature = "testsuite")] {
        pub mod tests;
        use crate::tests::*;
    } else {
        pub mod generic;
        use crate::generic::*;
    }
}

#[cfg(all(feature = "turbo", feature = "micro"))]
compile_error!(
    "enabling the 'micro' feature takes precedent over the 'turbo' feature,
     as both control memory buffer size"
);

cfg_if::cfg_if! {
    //
    // The "micro" feature denotes a minimal RAM footprint.  Note that this
    // will restrict Hiffy consumers to functions that return less than 64
    // bytes (minus overhead).  As of this writing, no Idol call returns more
    // than this -- but this will mean that I2cRead functionality that induces
    // a bus or device scan will result in a return stack overflow.
    // (Fortunately, this failure mode is relatively crisp for the consumer.)
    //
    if #[cfg(feature = "micro")] {
        const HIFFY_DATA_SIZE: usize = 256;
        const HIFFY_TEXT_SIZE: usize = 256;
        const HIFFY_RSTACK_SIZE: usize = 64;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    } else if #[cfg(feature = "turbo")] {
        //
        // go-faster mode
        //
        const HIFFY_DATA_SIZE: usize = 20_480;
        const HIFFY_TEXT_SIZE: usize = 4096;
        const HIFFY_RSTACK_SIZE: usize = 2048;

        /// Number of "scratch" bytes available to Hiffy programs. Humility uses this
        /// to deliver data used by some operations. This number can be increased at
        /// the cost of RAM.
        const HIFFY_SCRATCH_SIZE: usize = 1024;
    } else if #[cfg(any(target_board = "donglet-g031", target_board = "oxcon2023g0"))] {
        const HIFFY_DATA_SIZE: usize = 256;
        const HIFFY_TEXT_SIZE: usize = 256;
        const HIFFY_RSTACK_SIZE: usize = 2048;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    } else {
        const HIFFY_DATA_SIZE: usize = 2_048;
        const HIFFY_TEXT_SIZE: usize = 2048;
        const HIFFY_RSTACK_SIZE: usize = 2048;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    }
}

//
// These HIFFY_* global variables constitute the interface with Humility;
// they should not be altered without modifying Humility as well.
//
// - [`HIFFY_TEXT`]       => Program text for HIF operations
// - [`HIFFY_DATA`]       => Binary data from the caller
// - [`HIFFY_RSTACK`]     => HIF return stack
// - [`HIFFY_SCRATCH`]    => Scratch space for hiffy functions; debugger reads
//                           its size but does not modify it
// - [`HIFFY_REQUESTS`]   => Count of succesful requests
// - [`HIFFY_ERRORS`]     => Count of HIF execution failures
// - [`HIFFY_FAILURE`]    => Most recent HIF failure, if any
// - [`HIFFY_KICK`]       => Variable that will be written to to indicate that
//                           [`HIFFY_TEXT`] contains valid program text
// - [`HIFFY_READY`]      => Variable that will be non-zero iff the HIF
//                           execution engine is waiting to be kicked
//
// We are making the following items "no mangle" and "pub" to hint to the
// compiler that they are "exposed", and may be written (spookily) outside the
// scope of Rust itself. The aim is to prevent the optimizer from *assuming* the
// contents of these buffers will remain unchanged between accesses, as they
// will be written directly by the debugger.
//
// Below, we use atomic ordering (e.g. Acquire and Release) to inhibit
// compile- and run-time re-ordering around the explicit sequencing performed
// by the HIFFY_READY, HIFFY_KICK, HIFFY_REQUESTS, and HIFFY_ERRORS that are
// used to arbitrate shared access between the debugger and this software task.
//
// We assume that Hubris and Humility are cooperating, using the following state
// machines to avoid conflicting accesses:
// ┌─────────────────────────────────────────────────────────────────────────────────┐
// │                                                                                 │
// │                                        KICK == 0                                │
// │                    ┌────────────────────────────────────────────────┐           │
// │                    │                                                │           │
// │                    │                                       ┌─────────────────┐  │
// │  ┌─────────┐       ▽  ┌─────────────────┐     ┌───────┐    │ Write READY = 0 │  │
// │  │ Startup │──┬────┴─▷│ Write READY = 1 │────▷│ Sleep │───▷│ Read KICK       │  │
// │  └─────────┘  │       └─────────────────┘     └───────┘    │                 │  │
// │               │                                            └─────────────────┘  │
// │               │                                                     │           │
// │               │  ┌───────────────────────────────┐                  ▽           │
// │               │  │ Read REQUESTS                 │ Success ┌─────────────────┐  │
// │               ├──│ Write REQUESTS = REQUESTS + 1 │◁────┐   │ Write KICK = 0  │  │
// │               │  └───────────────────────────────┘     ├───│ Execute script  │  │
// │               │  ┌───────────────────────────────┐     │   │                 │  │
// │               │  │ Read ERRORS                   │     │   └─────────────────┘  │
// │               └──│ Write ERRORS = ERRORS + 1     │◁────┘                        │
// │                  └───────────────────────────────┘ Failure                      │
// │ ┌────────────┐                                                                  │
// └─┤ Hiffy Task ├──────────────────────────────────────────────────────────────────┘
//   └────────────┘
// ┌─────────────────────────────────────────────────────────────────────────────────┐
// │                                                                                 │
// │                                ┌────────────────┐                ┌────────┐     │
// │          ┌────────┐ READY == 1 │ Read REQUEST   │   REQUEST or   │ Read   │     │
// │        ┌▷│  Idle  │───────────▷│ Read ERRORS    │───────────────▷│ RESULT │     │
// │        │ └────────┘            │ Write KICK = 1 │ ERRORS changed │        │     │
// │        │                       └────────────────┘                └────────┘     │
// │        │                                                              │         │
// │        └──────────────────────────────────────────────────────────────┘         │
// │ ┌──────────┐                                                                    │
// └─┤ Humility ├────────────────────────────────────────────────────────────────────┘
//   └──────────┘
#[unsafe(no_mangle)]
pub static mut HIFFY_TEXT: [u8; HIFFY_TEXT_SIZE] = [0; HIFFY_TEXT_SIZE];
#[unsafe(no_mangle)]
pub static mut HIFFY_DATA: [u8; HIFFY_DATA_SIZE] = [0; HIFFY_DATA_SIZE];
#[unsafe(no_mangle)]
pub static mut HIFFY_RSTACK: [u8; HIFFY_RSTACK_SIZE] = [0; HIFFY_RSTACK_SIZE];

pub static HIFFY_SCRATCH: StaticCell<[u8; HIFFY_SCRATCH_SIZE]> =
    StaticCell::new([0; HIFFY_SCRATCH_SIZE]);

#[unsafe(no_mangle)]
pub static HIFFY_REQUESTS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_ERRORS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_KICK: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_READY: AtomicU32 = AtomicU32::new(0);

#[unsafe(no_mangle)]
pub static mut HIFFY_FAILURE: Option<Failure> = None;

// We deliberately export the HIF version numbers to allow Humility to
// fail cleanly if its HIF version does not match our own.
//
// Note that `#[unsafe(no_mangle)]` does not preserve these values through the
// linker, so we used `#[used]` instead.  They are not used by any code, so
// there's no safety concerns.
#[used]
pub static HIFFY_VERSION_MAJOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MAJOR);
#[used]
pub static HIFFY_VERSION_MINOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MINOR);
#[used]
pub static HIFFY_VERSION_PATCH: AtomicU32 = AtomicU32::new(HIF_VERSION_PATCH);

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut sleep_ms = 250;
    let mut sleeps = 0;
    let mut stack = [None; 32];
    const NLABELS: usize = 4;

    loop {
        HIFFY_READY.store(1, Ordering::Relaxed);
        hl::sleep_for(sleep_ms);
        HIFFY_READY.store(0, Ordering::Relaxed);

        // Humility writes `1` to `HIFFY_KICK`
        if HIFFY_KICK.load(Ordering::Acquire) == 0 {
            sleeps += 1;

            // Exponentially backoff our sleep value, but no more than 250ms
            if sleeps == 10 {
                sleep_ms = core::cmp::min(sleep_ms * 10, 250);
                sleeps = 0;
            }

            continue;
        }

        //
        // Whenever we have been kicked, we adjust our timeout down to 1ms,
        // from which we will exponentially backoff
        //
        HIFFY_KICK.store(0, Ordering::Release);
        sleep_ms = 1;
        sleeps = 0;

        let check = |offset: usize, op: &Op| -> Result<(), Failure> {
            trace_execute(offset, *op);
            Ok(())
        };

        let rv = {
            // Dummy object to bind references to a non-static lifetime
            let lifetime = ();

            // SAFETY: We construct references from our pointers with a limited
            // (non-static) lifetime, so they can't escape this block.  We are
            // in single-threaded code, so no one else can read or write to
            // static memory.  While the HIF program is running, the debugger is
            // only reading from `HIFFY_REQUESTS` and `HIFFY_ERRORS`; it is not
            // writing to any locations in memory.  See the diagram above for
            // Hubris / Humility coordination.
            let (text, data, rstack) = unsafe {
                (
                    bind_lifetime_ref(&lifetime, &raw const HIFFY_TEXT),
                    bind_lifetime_ref(&lifetime, &raw const HIFFY_DATA),
                    bind_lifetime_mut(&lifetime, &raw mut HIFFY_RSTACK),
                )
            };
            execute::<_, NLABELS>(
                text,
                HIFFY_FUNCS,
                data,
                &mut stack,
                rstack,
                &mut *HIFFY_SCRATCH.borrow_mut(),
                check,
            )
        };

        match rv {
            Ok(_) => {
                let prev = HIFFY_REQUESTS.load(Ordering::Relaxed);
                HIFFY_REQUESTS.store(prev.wrapping_add(1), Ordering::Release);
                trace_success();
            }
            Err(failure) => {
                // SAFETY: We are in single-threaded code and the debugger will
                // not be reading HIFFY_FAILURE until HIFFY_ERRORS is
                // incremented below.  See the diagram above for Hubris /
                // Humility coordination.
                unsafe {
                    HIFFY_FAILURE = Some(failure);
                }
                let prev = HIFFY_ERRORS.load(Ordering::Relaxed);
                HIFFY_ERRORS.store(prev.wrapping_add(1), Ordering::Release);
                trace_failure(failure);
            }
        }
    }
}

/// Converts an array pointer to a shared reference with a particular lifetime
///
/// # Safety
/// `ptr` must point to a valid, aligned, initialized `[u8; N]`.
/// The referent must not be mutated while the returned reference is live.
#[expect(clippy::needless_lifetimes)] // gotta make it obvious
unsafe fn bind_lifetime_ref<'a, const N: usize>(
    _: &'a (),
    array: *const [u8; N],
) -> &'a [u8; N] {
    // SAFETY: converting from pointer to reference is safe given the function's
    // safety conditions (listed in docstring)
    unsafe { array.as_ref().unwrap_lite() }
}

/// Converts an array pointer to a mutable reference with a particular lifetime
///
/// # Safety
/// `ptr` must point to a valid, aligned, initialized `[u8; N]`.
/// The referent must not be mutated while the returned reference is live.
#[expect(clippy::needless_lifetimes, clippy::mut_from_ref)]
unsafe fn bind_lifetime_mut<'a, const N: usize>(
    _: &'a (),
    array: *mut [u8; N],
) -> &'a mut [u8; N] {
    // SAFETY: converting from pointer to reference is safe given the function's
    // safety conditions (listed in docstring)
    unsafe { array.as_mut().unwrap_lite() }
}
