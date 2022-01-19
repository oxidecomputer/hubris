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
extern crate stm32h7;

use stm32h7::stm32h753 as device;

use drv_stm32h7_startup::ClockConfig;

use cortex_m_rt::entry;
use kern::app::App;

extern "C" {
    static hubris_app_table: App;
    static mut __sheap: u8;
    static __eheap: u8;
}

#[entry]
fn main() -> ! {
    // We have an 8MHz external crystal.
    drv_stm32h7_startup::system_init(ClockConfig {
        source: drv_stm32h7_startup::ClockSource::ExternalCrystal,
        // 8MHz HSE freq is within VCO input range of 2-16, so, DIVM=1 to bypass
        // the prescaler.
        divm: 1,
        // VCO must tolerate an 8MHz input range:
        vcosel: device::rcc::pllcfgr::PLL1VCOSEL_A::WIDEVCO,
        pllrange: device::rcc::pllcfgr::PLL1RGE_A::RANGE8,
        // DIVN governs the multiplication of the VCO input frequency to produce
        // the intermediate frequency. We want an IF of 800MHz, or a
        // multiplication of 100x.
        //
        // We subtract 1 to get the DIVN value because the PLL effectively adds
        // one to what we write.
        divn: 100 - 1,
        // P is the divisor from the VCO IF to the system frequency. We want
        // 400MHz, so:
        divp: device::rcc::pll1divr::DIVP1_A::DIV2,
        // Q produces kernel clocks; we set it to 200MHz:
        divq: 4 - 1,
        // R is mostly used by the trace unit and we leave it fast:
        divr: 2 - 1,

        // We run the CPU at the full core rate of 400MHz:
        cpu_div: device::rcc::d1cfgr::D1CPRE_A::DIV1,
        // We down-shift the AHB by a factor of 2, to 200MHz, to meet its
        // constraints:
        ahb_div: device::rcc::d1cfgr::HPRE_A::DIV2,
        // We configure all APB for 100MHz. These are relative to the AHB
        // frequency.
        apb1_div: device::rcc::d2cfgr::D2PPRE1_A::DIV2,
        apb2_div: device::rcc::d2cfgr::D2PPRE2_A::DIV2,
        apb3_div: device::rcc::d1cfgr::D1PPRE_A::DIV2,
        apb4_div: device::rcc::d3cfgr::D3PPRE_A::DIV2,

        // Flash runs at 200MHz: 2WS, 2 programming cycles. See reference manual
        // Table 13.
        flash_latency: 2,
        flash_write_delay: 2,
    });

    const CYCLES_PER_MS: u32 = 400_000;

    unsafe {
        let heap_size =
            (&__eheap as *const _ as usize) - (&__sheap as *const _ as usize);
        kern::startup::start_kernel(
            &hubris_app_table,
            (&mut __sheap) as *mut _,
            heap_size,
            CYCLES_PER_MS,
        )
    }
}
