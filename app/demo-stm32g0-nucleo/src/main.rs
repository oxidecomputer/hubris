// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(not(any(
    feature = "panic-itm",
    feature = "panic-semihosting",
    feature = "panic-halt"
)))]
compile_error!("Must have one of panic-{itm,semihosting,halt} enabled");

// Panic behavior controlled by Cargo features:
#[cfg(feature = "panic-halt")]
extern crate panic_halt;
#[cfg(feature = "panic-itm")]
extern crate panic_itm; // breakpoint on `rust_begin_unwind` to catch panics
#[cfg(feature = "panic-semihosting")]
extern crate panic_semihosting; // requires a debugger

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32g0;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    const CYCLES_PER_MS: u32 = 16_000;

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
