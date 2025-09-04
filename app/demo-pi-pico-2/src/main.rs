// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
use rp235x_pac as _;

use cortex_m_rt::entry;

/// Image definition to set up the chip for booting
///
/// See datasheet 5.1.4 Image Definitions for more. These specific values come from
/// section 5.9.5. Minimum viable image metadata.
#[link_section = ".image_def"]
#[used]
pub static RP235X_IMAGE_DEF_ARM_MIN: [u32; 5] = [
    0xFFFF_DED3, // START
    0x1021_0142, // PICOBIN_BLOCK_ITEM_1BS_IMAGE_TYPE, (EXE | S-mode | ARM | RP2350)
    0x0000_01FF, // PICOBIN_BLOCK_ITEM_2BS_LAST, (size=1 word)
    0x0000_0000, // next = self
    0xAB12_3579, // END
];

#[entry]
fn main() -> ! {
    let p = unsafe { rp235x_pac::Peripherals::steal() };

    p.RESETS.reset().modify(|_, w| w.io_bank0().clear_bit());
    while !p.RESETS.reset_done().read().io_bank0().bit() {}

    // TODO fix/update this for RP2350
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

    unsafe { kern::startup::start_kernel(cycles_per_ms) }
}
