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
//
// TODO: Hiffy is using unsafe and static mut in ways that are not obviously
// sound. This became a warning in early 2024. In the interest of preventing
// regressions in everything _else_ I'm suppressing the warning here so we can
// turn Clippy back on. If you're reading this, this file is potentially unsound
// and needs attention!
//
#![allow(static_mut_refs)]

// This trait may not be needed, if compiling for a non-armv6m target.
#[allow(unused_imports)]
use armv6m_atomic_hack::AtomicU32Ext;
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

///
/// These HIFFY_* global variables constitute the interface with Humility;
/// they should not be altered without modifying Humility as well.
///
/// - [`HIFFY_TEXT`]       => Program text for HIF operations
/// - [`HIFFY_DATA`]       => Binary data from the caller
/// - [`HIFFY_RSTACK`]     => HIF return stack
/// - [`HIFFY_SCRATCH`]    => Scratch space for hiffy functions
/// - [`HIFFY_REQUESTS`]   => Count of succesful requests
/// - [`HIFFY_ERRORS`]     => Count of HIF execution failures
/// - [`HIFFY_FAILURE`]    => Most recent HIF failure, if any
/// - [`HIFFY_KICK`]       => Variable that will be written to to indicate that
///                           [`HIFFY_TEXT`] contains valid program text
/// - [`HIFFY_READY`]      => Variable that will be non-zero iff the HIF
///                           execution engine is waiting to be kicked
///
static mut HIFFY_TEXT: [u8; HIFFY_TEXT_SIZE] = [0; HIFFY_TEXT_SIZE];
static mut HIFFY_DATA: [u8; HIFFY_DATA_SIZE] = [0; HIFFY_DATA_SIZE];
static mut HIFFY_RSTACK: [u8; HIFFY_RSTACK_SIZE] = [0; HIFFY_RSTACK_SIZE];

static HIFFY_SCRATCH: StaticCell<[u8; HIFFY_SCRATCH_SIZE]> =
    StaticCell::new([0; HIFFY_SCRATCH_SIZE]);

#[used]
static HIFFY_REQUESTS: AtomicU32 = AtomicU32::new(0);
#[used]
static HIFFY_ERRORS: AtomicU32 = AtomicU32::new(0);
#[used]
static HIFFY_KICK: AtomicU32 = AtomicU32::new(0);
#[used]
static HIFFY_READY: AtomicU32 = AtomicU32::new(0);

#[used]
static mut HIFFY_FAILURE: Option<Failure> = None;

///
/// We deliberately export the HIF version numbers to allow Humility to
/// fail cleanly if its HIF version does not match our own.
///
#[used]
static HIFFY_VERSION_MAJOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MAJOR);
#[used]
static HIFFY_VERSION_MINOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MINOR);
#[used]
static HIFFY_VERSION_PATCH: AtomicU32 = AtomicU32::new(HIF_VERSION_PATCH);

#[export_name = "main"]
fn main() -> ! {
    let mut sleep_ms = 250;
    let mut sleeps = 0;
    let mut stack = [None; 32];
    const NLABELS: usize = 4;

    //
    // Sadly, there seems to be no other way to force these variables to
    // not be eliminated...
    //
    HIFFY_VERSION_MAJOR.fetch_add(0, Ordering::SeqCst);
    HIFFY_VERSION_MINOR.fetch_add(0, Ordering::SeqCst);
    HIFFY_VERSION_PATCH.fetch_add(0, Ordering::SeqCst);

    loop {
        HIFFY_READY.fetch_add(1, Ordering::SeqCst);
        hl::sleep_for(sleep_ms);
        HIFFY_READY.fetch_sub(1, Ordering::SeqCst);

        if HIFFY_KICK.load(Ordering::SeqCst) == 0 {
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
        HIFFY_KICK.fetch_sub(1, Ordering::SeqCst);
        sleep_ms = 1;
        sleeps = 0;

        // TODO without a safety comment explaining why these are safe, it is
        // not clear if this is sound, do _not_ "fix" this by slapping on an
        // addr_of_mut! without further analysis!
        let text = unsafe { &HIFFY_TEXT };
        let data = unsafe { &HIFFY_DATA };
        let rstack = unsafe { &mut HIFFY_RSTACK[0..] };

        let check = |offset: usize, op: &Op| -> Result<(), Failure> {
            trace_execute(offset, *op);
            Ok(())
        };

        // XXX: workaround for false-positive due to rust-lang/rust-clippy#9126
        #[allow(clippy::explicit_auto_deref)]
        let rv = execute::<_, NLABELS>(
            text,
            HIFFY_FUNCS,
            data,
            &mut stack,
            rstack,
            &mut *HIFFY_SCRATCH.borrow_mut(),
            check,
        );

        match rv {
            Ok(_) => {
                HIFFY_REQUESTS.fetch_add(1, Ordering::SeqCst);
                trace_success();
            }
            Err(failure) => {
                HIFFY_ERRORS.fetch_add(1, Ordering::SeqCst);
                unsafe {
                    HIFFY_FAILURE = Some(failure);
                }

                trace_failure(failure);
            }
        }
    }
}
