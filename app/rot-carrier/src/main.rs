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

use abi::ImageHeader;
use core::mem::MaybeUninit;
use cortex_m_rt::entry;
use lpc55_pac as device;

// This is updated by build scripts (which is why this is marked as no_mangle)
// Although we don't access any fields of the header from hubris right now, it
// is safer to treat this as MaybeUninit in case we need to do so in the future.
#[used]
#[no_mangle]
#[link_section = ".image_header"]
static HEADER: MaybeUninit<ImageHeader> = MaybeUninit::uninit();

#[entry]
fn main() -> ! {
    // Confusingly, UM11126 lists the reset values of the clock
    // configuration registers as setting MAINCLKA = MAINCLKB =
    // FRO_12MHz.  This is true but only before the Boot ROM runs.  Per
    // §10.2.1, the Boot ROM will read the CMPA to determine what speed
    // to run the cores at with options for 48MHz, 96MHz, and
    // NMPA.SYSTEM_SPEED_CODE. With no extra settings the ROM uses
    // the NMPA setting.
    //
    // Importantly, there is an extra divider to determine the CPU
    // speed which divides the MAINCLKA = 96MHz by 2 to get 48MHz.
    const CYCLES_PER_MS: u32 = 48_000;

    unsafe {
        //
        // To allow for SWO (the vector for ITM output), we must explicitly
        // enable it on pin0_10.
        //
        let iocon = &*device::IOCON::ptr();
        iocon.pio0_10.modify(|_, w| w.func().alt6());

        // SWO is clocked indepdently of the CPU. Match the CPU
        // settings by setting the divider
        let syscon = &*device::SYSCON::ptr();
        syscon.traceclkdiv.modify(|_, w| w.div().bits(1));

        kern::startup::start_kernel(CYCLES_PER_MS)
    }
}
