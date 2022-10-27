// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32h7;

use stm32h7::stm32h753 as device;

use drv_stm32h7_startup::ClockConfig;

use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    system_init();

    const CYCLES_PER_MS: u32 = 400_000;

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}

fn system_init() {
    let cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    // We want to measure PG0-2 to determine if we're running on the correct
    // board.  On rev A, these pins are left floating; on later revisions, they
    // are pulled either high or low.
    //
    // This code matches that in gimlet/src/main.rs; see detailed comments over
    // there about how times were calculated.

    // Un-gate the clock to GPIO bank G.
    p.RCC.ahb4enr.modify(|_, w| w.gpiogen().set_bit());
    cortex_m::asm::dsb();

    // PG2:0 are already inputs after reset, but without any pull resistors.
    #[rustfmt::skip]
    p.GPIOG.moder.modify(|_, w| w
        .moder0().input()
        .moder1().input()
        .moder2().input());
    // Enable the pullups.
    #[rustfmt::skip]
    p.GPIOG.pupdr.modify(|_, w| w
        .pupdr0().pull_up()
        .pupdr1().pull_up()
        .pupdr2().pull_up());

    // Wait for pins to charge / discharge (see comment in gimlet/src/main.rs
    // for the actual calculations).
    cortex_m::asm::delay(155 * 2);
    let rev = p.GPIOG.idr.read().bits() & 0b111;

    cfg_if::cfg_if! {
        if #[cfg(target_board = "psc-a")] {
            let expected_rev = 0b111;
        } else if #[cfg(target_board = "psc-b")] {
            let expected_rev = 0b001;
        } else {
            compile_error!("not a recognized psc board")
        }
    }
    assert_eq!(rev, expected_rev);

    drv_stm32h7_startup::system_init_custom(
        cp,
        p,
        ClockConfig {
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
        },
    );
}
