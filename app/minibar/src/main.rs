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

    // Check the package we've been flashed on. Minibar boards use BGA240.
    // Gimletlet boards are very similar but use QFPs. This is designed to fail
    // a Minibar firmware that was accidentally flashed onto a Gimletlet.
    //
    // We need to turn the SYSCFG block on to do this.
    p.RCC.apb4enr.modify(|_, w| w.syscfgen().enabled());
    cortex_m::asm::dsb();
    // Now, we can read the appropriately-named package register to find out
    // what package we're on.
    match p.SYSCFG.pkgr.read().pkg().bits() {
        0b1000 => {
            // TFBGA240, yay
        }
        _ => {
            // uh
            panic!();
        }
    }

    // Minibar has resistors strapping three pins to indicate the board
    // revision.
    //
    // We read the board version very early in boot to try and detect the
    // firmware being flashed on the wrong board. In particular, we read the
    // version _before_ setting up the clock tree below, just in case we change
    // the crystal configuration in a subsequent rev.
    //
    // Note that the firmware _does not_ adapt to different board revs. We still
    // require different firmware per revision; this check serves to detect if
    // you've flashed the wrong one, only.
    //
    // The revision is on the following pins:
    // - HCV_CODE_2: PK5
    // - HCV_CODE_1: PK6
    // - HCV_CODE_0: PK7

    // Un-gate the clock to GPIO bank K.
    p.RCC.ahb4enr.modify(|_, w| w.gpioken().set_bit());
    cortex_m::asm::dsb();
    // PK5,6,6 are already inputs after reset, but without any pull resistors.
    #[rustfmt::skip]
    p.GPIOK.moder.modify(|_, w| w
        .moder5().input()
        .moder6().input()
        .moder7().input());
    // Enable the pullups.
    #[rustfmt::skip]
    p.GPIOK.pupdr.modify(|_, w| w
        .pupdr5().pull_up()
        .pupdr6().pull_up()
        .pupdr7().pull_up());

    // TODO: fill in timing justification here based on Sidecar's schematic.
    cortex_m::asm::delay(2000);

    // Build the full ID
    let rev = p.GPIOK.idr.read().bits();
    let rev = [7, 6, 5]
        .iter()
        .enumerate()
        .map(|(i, bit)| if (rev & (1 << bit)) != 0 { 1 << i } else { 0 })
        .fold(0, |acc, v| acc | v);

    cfg_if::cfg_if! {
        if #[cfg(target_board = "minibar")] {
            let expected_rev = 0b001;
        } else {
            compile_error!("not a recognized minibar board")
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
