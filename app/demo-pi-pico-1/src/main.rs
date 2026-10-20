// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// make sure the vector table gets linked in
extern crate rp2040_hal;

use hal::pac;
use rp2040_hal as hal;

#[unsafe(link_section = ".boot2")]
#[used]
pub static BOOT_LOADER: [u8; 256] = hal::boot::BOOT_LOADER_W25Q080;

#[hal::entry]
fn main() -> ! {
    // SAFETY: This is the only time we're calling this?
    let p = unsafe { pac::Peripherals::steal() };

    // reset the io_bank and wait for it to be done resetting
    hal::reset_hw!(p.RESETS, [io_bank0]);

    // figure out what clock speed to use
    let cycles_per_ms = if p.CLOCKS.clk_sys_ctrl().read().src().is_clk_ref() {
        // This is the reset state, so we'll assume we launched directly from
        // flash running on the ROSC.
        6_000 // ish
    } else {
        // This is _not_ the reset state, so we'll assume that the pico-debug
        // resident debugger has reconfigured things to run off the 48 MHz USB
        // clock.
        48_000
    };
    unsafe { kern::startup::start_kernel(cycles_per_ms) };
}
