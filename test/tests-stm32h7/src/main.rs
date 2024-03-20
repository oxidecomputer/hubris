// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32h7;

use cortex_m_rt::entry;
use drv_stm32h7_startup::{system_init, ClockConfig};

#[entry]
fn main() -> ! {
    const CYCLES_PER_MS: u32 = 64_000;
    const CLOCKS: ClockConfig = ClockConfig {
        // The Nucleo board doesn't include an external crystal, so we
        // derive clocks from the HSI64 oscillator.
        source: drv_stm32h7_startup::ClockSource::Hsi64,
        // We don't divide it down for the CPU or any buses.
        cpu_div: device::rcc::d1cfgr::D1CPRE_A::DIV1,
        ahb_div: device::rcc::d1cfgr::HPRE_A::DIV1,
        apb1_div: device::rcc::d2cfgr::D2PPRE1_A::DIV1,
        apb2_div: device::rcc::d2cfgr::D2PPRE2_A::DIV1,
        apb3_div: device::rcc::d1cfgr::D1PPRE_A::DIV1,
        apb4_div: device::rcc::d3cfgr::D3PPRE_A::DIV1,
        // Flash can keep up with the full rate.
        flash_latency: 0,
        flash_write_delay: 0,

        // PLL is not used, fields below are irrelevant and contain
        // placeholders.
        divm: 0,
        vcosel: device::rcc::pllcfgr::PLL1VCOSEL_A::WIDEVCO,
        pllrange: device::rcc::pllcfgr::PLL1RGE_A::RANGE8,
        divn: 0,
        divp: device::rcc::pll1divr::DIVP1_A::DIV2,
        divq: 0,
        divr: 0,
    };

    system_init(CLOCKS);

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}
