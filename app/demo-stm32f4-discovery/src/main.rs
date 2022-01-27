// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(not(any(feature = "panic-itm", feature = "panic-semihosting")))]
compile_error!(
    "Must have either feature panic-itm or panic-semihosting enabled"
);

// Panic behavior controlled by Cargo features:
#[cfg(feature = "panic-itm")]
extern crate panic_itm; // breakpoint on `rust_begin_unwind` to catch panics
#[cfg(feature = "panic-semihosting")]
extern crate panic_semihosting; // requires a debugger

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
#[cfg(feature = "stm32f3")]
extern crate stm32f3;
#[cfg(feature = "stm32f4")]
extern crate stm32f4;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    // Default boot speed, until we bother raising it:
    #[cfg(feature = "stm32f3")]
    const CYCLES_PER_MS: u32 = 8_000;
    #[cfg(feature = "stm32f4")]
    const CYCLES_PER_MS: u32 = 16_000;

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
