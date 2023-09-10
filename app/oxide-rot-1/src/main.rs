// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use lpc55_romapi::{set_hashcrypt_handler, set_hashcrypt_handler_to_rom};
use lpc55_rot_startup::{get_clock_speed, startup};
use unwrap_lite::UnwrapLite;

#[entry]
fn main() -> ! {
    let core_peripherals = cortex_m::Peripherals::take().unwrap_lite();
    let peripherals = lpc55_pac::Peripherals::take().unwrap_lite();

    let (cycles_per_ms, _div) = get_clock_speed(&peripherals);

    // Pre-main code makes calls to the ROM-based signature
    // verification routines and requires its own HASHCRYPT IRQ handler.
    set_hashcrypt_handler_to_rom();

    startup(&core_peripherals, &peripherals);

    // Once the kernel is started, the normal HASHCRYPT IRQ handler needs to
    // be active.
    let irq_handler: unsafe extern "C" fn() -> () = kern::arch::DefaultHandler;
    set_hashcrypt_handler(irq_handler);

    unsafe { kern::startup::start_kernel(cycles_per_ms * 1_000) }
}
