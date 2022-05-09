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
use rp2040_pac as _;

#[link_section = ".boot_loader"]
#[used]
pub static BOOT_LOADER: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    let p = unsafe { rp2040_pac::Peripherals::steal() };

    p.RESETS.reset.modify(|_, w| w.io_bank0().clear_bit());
    while !p.RESETS.reset_done.read().io_bank0().bit() {}

    p.SIO.gpio_oe_set.write(|w| unsafe { w.bits(1 << 25) });
    p.SIO.gpio_out_set.write(|w| unsafe { w.bits(1 << 25) });

    p.IO_BANK0.gpio[25].gpio_ctrl.write(|w| w.funcsel().sio());



    const CYCLES_PER_MS: u32 = 6_000; // ish

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
