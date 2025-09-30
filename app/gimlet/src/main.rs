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

#[cfg(feature = "traptrace")]
mod tracing;

#[entry]
fn main() -> ! {
    system_init();

    const CYCLES_PER_MS: u32 = 400_000;

    #[cfg(feature = "traptrace")]
    kern::profiling::configure_events_table(tracing::table());

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}

fn system_init() {
    let cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    // Check the package we've been flashed on. Gimlet boards use BGA240.
    // Gimletlet boards are very similar but use QFPs. This is designed to fail
    // a Gimlet firmware that was accidentally flashed onto a Gimletlet.
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

    // Gimlet, starting at Rev B, has resistors strapping three pins to indicate
    // the board revision. Rev A left the pins floating, so, we have a
    // backwards-compatibility scheme where we use our internal pull-up
    // resistors, causing Rev A to read as 0b111.
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
    // The revision is on pins PG[2:0], with PG2 as the MSB.

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
    // We are now charging up the board revision traces through the ~40kR
    // internal pullup resistors. The floating trace is the biggie, since we're
    // responsible for putting in any charge that we detect. While the
    // capacitance should be low, it's not zero, and even running at the reset
    // frequency of 64MHz, we are very much racing the trace charging.
    //
    // Assuming 50pF for the trace plus the iCE40's tristated input on the far
    // end, we get
    //
    // RC = 40 kR * 50 pF = 2e-6
    // Time to reach Vil of 2.31 V (0.7 VDD) = 2.405 us
    //
    // Maximum speed of 64MHz oscillator after ST manufacturing calibration, per
    // the datasheet, is 64.3 MHz.
    //
    // 2.405us @ 64.3MHz = 154.64 cycles ~= 155 cycles.
    //
    // The cortex_m delay routine is written for single-issue simple cores and
    // is simply wrong on the M7 (they know this). So, let's conservatively pad
    // it by a factor of 2.
    cortex_m::asm::delay(155 * 2);

    // Okay! What does the fox^Wpins say?
    let rev = p.GPIOG.idr.read().bits() & 0b111;

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gimlet-b")] {
            let expected_rev = 0b001;
        } else if #[cfg(target_board = "gimlet-c")] {
            let expected_rev = 0b010;
        } else if #[cfg(target_board = "gimlet-d")] {
            let expected_rev = 0b011;
        } else if #[cfg(target_board = "gimlet-e")] {
            let expected_rev = 0b111; // hardware-gimlet#1952
        } else if #[cfg(target_board = "gimlet-f")] {
            let expected_rev = 0b101;
        } else {
            compile_error!("not a recognized gimlet board")
        }
    }

    assert_eq!(rev, expected_rev);

    // Do most of the setup with the common implementation.
    let p = drv_stm32h7_startup::system_init_custom(
        cp,
        p,
        ClockConfig {
            source: drv_stm32h7_startup::ClockSource::ExternalCrystal,
            // 8MHz HSE freq is within VCO input range of 2-16, so, DIVM=1 to bypass
            // the prescaler.
            divm: 1,
            // VCO must tolerate an 8MHz input range:
            vcosel: device::rcc::pllcfgr::PLL1VCOSEL_A::WideVco,
            pllrange: device::rcc::pllcfgr::PLL1RGE_A::Range8,
            // DIVN governs the multiplication of the VCO input frequency to produce
            // the intermediate frequency. We want an IF of 800MHz, or a
            // multiplication of 100x.
            //
            // We subtract 1 to get the DIVN value because the PLL effectively adds
            // one to what we write.
            divn: 100 - 1,
            // P is the divisor from the VCO IF to the system frequency. We want
            // 400MHz, so:
            divp: device::rcc::pll1divr::DIVP1_A::Div2,
            // Q produces kernel clocks; we set it to 200MHz:
            divq: 4 - 1,
            // R is mostly used by the trace unit and we leave it fast:
            divr: 2 - 1,

            // We run the CPU at the full core rate of 400MHz:
            cpu_div: device::rcc::d1cfgr::D1CPRE_A::Div1,
            // We down-shift the AHB by a factor of 2, to 200MHz, to meet its
            // constraints:
            ahb_div: device::rcc::d1cfgr::HPRE_A::Div2,
            // We configure all APB for 100MHz. These are relative to the AHB
            // frequency.
            apb1_div: device::rcc::d2cfgr::D2PPRE1_A::Div2,
            apb2_div: device::rcc::d2cfgr::D2PPRE2_A::Div2,
            apb3_div: device::rcc::d1cfgr::D1PPRE_A::Div2,
            apb4_div: device::rcc::d3cfgr::D3PPRE_A::Div2,

            // Flash runs at 200MHz: 2WS, 2 programming cycles. See reference manual
            // Table 13.
            flash_latency: 2,
            flash_write_delay: 2,
        },
    );

    // Gimlet uses PA0_C instead of PA0. Flip this.
    p.SYSCFG.pmcr.modify(|_, w| {
        w.pa0so()
            .clear_bit()
            .pa1so()
            .set_bit()
            .pc2so()
            .set_bit()
            .pc3so()
            .set_bit()
    });
}
