// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use lpc55_pac as device;

// When we're secure we don't have access to read the CMPA/NMPA where the
// official setting is stored, emulate what the clock driver does instead
fn get_clock_speed() -> (u32, u8) {
    // We need to set the clock speed for flash programming to work
    // properly. Reading it out of syscon is less error prone than
    // trying to compile it in at build time

    let syscon = unsafe { &*lpc55_pac::SYSCON::ptr() };

    let a = syscon.mainclksela.read().bits();
    let b = syscon.mainclkselb.read().bits();
    let div = syscon.ahbclkdiv.read().bits();

    // corresponds to FRO 96 MHz, see 4.5.34 in user manual
    const EXPECTED_MAINCLKSELA: u32 = 3;
    // corresponds to Main Clock A, see 4.5.45 in user manual
    const EXPECTED_MAINCLKSELB: u32 = 0;

    // We expect the 96MHz clock to be used based on the ROM.
    // If it's not there are probably more (bad) surprises coming
    // and panicking is reasonable
    if a != EXPECTED_MAINCLKSELA || b != EXPECTED_MAINCLKSELB {
        panic!();
    }

    if div == 0 {
        (96, div as u8)
    } else {
        (48, div as u8)
    }
}

#[entry]
fn main() -> ! {
    let (cycles_per_ms, div) = get_clock_speed();

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
        syscon.traceclkdiv.modify(|_, w| w.div().bits(div));

        kern::startup::start_kernel(cycles_per_ms * 1_000)
    }
}
