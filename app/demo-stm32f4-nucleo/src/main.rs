// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32f4;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    const CYCLES_PER_MS: u32 = 16_000;

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
