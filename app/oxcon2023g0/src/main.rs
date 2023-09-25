// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32g0;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    const CYCLES_PER_MS: u32 = 64_000;

    let rcc = unsafe { &*stm32g0::stm32g030::RCC::PTR };
    rcc.apbenr2.modify(|_, w| {
        w.syscfgen().set_bit();
        w
    });
    cortex_m::asm::dsb();

    let flash = unsafe { &*stm32g0::stm32g030::FLASH::PTR };
    flash.acr.modify(|_, w| unsafe { w.latency().bits(2) });

    // PLL settings we want are:
    // - SRC = 16 MHz oscillator
    // - M = PLL input division = /1 (so VCO input = 16 MHz)
    // - N = VCO multiplication factor = 8 (the lowest possible = 128 MHz)
    // - R = Output R division = /2 (lowest possible = 64 MHz)
    // - Output R goes to CPU
    rcc.pllsyscfgr.write(|w| {
        unsafe {
            w.pllsrc().bits(0b10); // HSI16, I promise
            w.pllm().bits(1 - 1);
            w.plln().bits(8); // _not_ an n-1 field
            w.pllr().bits(2 - 1);
        }
        w.pllren().set_bit();
        w
    });
    // Turn on the PLL and wait for it to stabilize.
    rcc.cr.modify(|_, w| {
        w.pllon().set_bit();
        w
    });
    while rcc.cr.read().pllrdy().bit_is_clear() {
        // spin.
    }
    rcc.cfgr.modify(|_, w| {
        unsafe {
            w.sw().bits(0b010);
        }
        w
    });
    while rcc.cfgr.read().sws().bits() != 0b010 {
        // spin.
    }

    let syscfg = unsafe { &*stm32g0::stm32g030::SYSCFG::PTR };
    syscfg.cfgr1.modify(|r, w| {
        // the PAC mapping of the PA9/PA10 remap bits is wrong. Do this by hand:
        unsafe {
            w.bits(r.bits() | 1 << 3 | 1 << 4);
        }
        w
    });

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
