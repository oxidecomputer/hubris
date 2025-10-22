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
    // the board revision.  These resistors pull the revision pins down to
    // ground; we also use the iCE40 FPGA's weak pull-up so that revision pins
    // without resistors are pulled high.
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
    //
    // In earlier firmware, we used the SP's internal pull-up resistors.
    // However, the combination of both FPGA and SP pull-up resistors was too
    // strong on certain hardware revisions, leading to marginal voltage
    // readings (see hubris#2255 for the gory details).
    //
    // Instead, we reset the FPGA, which puts its pins into their default
    // configuration (with weak pull-ups enabled).  Then, we wait for the pins
    // to charge before reading the values.  We have to reset the FPGA, because
    // there are images in the wild which configure those pins *without*
    // pull-ups; doing an update + warm reset from such an image would leave the
    // FPGA configured at this point in the code, without pull-ups.

    // Un-gate the clock to GPIO bank G (for revision ID), D (for FPGA reset),
    // and C (for sequencer PG)
    p.RCC.ahb4enr.modify(|_, w| {
        w.gpiogen().set_bit();
        w.gpioden().set_bit();
        w.gpiocen().set_bit()
    });
    cortex_m::asm::dsb();

    // Make PC6 (SEQ_REG_TO_SP_V3P3_PG) and PC7 (SEQ_REG_TO_SP_V1P2_PG) inputs,
    // then wait for both of them to go high.  We time out after 1M iterations
    // (with 100 cycles each), which is roughly 1.5s.
    p.GPIOC.moder.modify(|_, w| {
        w.moder6().input();
        w.moder7().input()
    });
    const SEQ_PG: u32 = 0b11 << 6;
    let mut seq_pg_okay = false;
    for _ in 0..1_000_000 {
        if p.GPIOC.idr.read().bits() & SEQ_PG == SEQ_PG {
            seq_pg_okay = true;
            break;
        } else {
            cortex_m::asm::delay(100);
        }
    }
    if !seq_pg_okay {
        panic!("timeout waiting for sequencer PG lines");
    }

    // Make CRESET (PD5) an output (initially high), then toggle it low to reset
    // the FPGA bitstream.  The minimum CRESET pulse is 200 ns, or 13 cycles,
    // but there's a 1µF capacitor on that line.  Let's assume we're discharging
    // the capacitor at 5 mA from 3V3; in that case, it will take 0.66 ms, or
    // 42K cycles.  We'll be conservative and pad it to 100K cycles.
    p.GPIOD.bsrr.write(|w| w.bs5().set());
    p.GPIOD.moder.modify(|_, w| w.moder5().output());
    p.GPIOD.bsrr.write(|w| w.br5().reset());
    cortex_m::asm::delay(100_000);

    p.GPIOG.moder.modify(|_, w| {
        w.moder0().input();
        w.moder1().input();
        w.moder2().input()
    });

    // We are now charging up the board revision traces through the iCE40's
    // pull-up, which delivers between 11 and 128 µA of current. The floating
    // trace is the biggie, since we're responsible for putting in any charge
    // that we detect. While the capacitance should be low, it's not zero, and
    // even running at the reset frequency of 64MHz, we are very much racing the
    // trace charging.
    //
    // Assuming 50pF for the trace plus the iCE40's tristated input on the far
    // end, we get
    //
    // V(t) = 1 / 50 pF * 10 µA * t
    // Time to reach Vil of 2.31 V (0.7 VDD) = 11.55 µs
    //
    // Maximum speed of 64MHz oscillator after ST manufacturing calibration, per
    // the datasheet, is 64.3 MHz.
    //
    // 11.55 µs @ 64.3MHz ~= 743 cycles
    //
    // The cortex_m delay routine is written for single-issue simple cores and
    // is simply wrong on the M7 (they know this). So, let's conservatively pad
    // it by a factor of 10.
    cortex_m::asm::delay(743 * 10);

    // Okay! What does the fox^Wpins say?
    let rev = p.GPIOG.idr.read().bits() & 0b111;

    // Pull CRESET high for a clean handoff to user code
    p.GPIOD.bsrr.write(|w| w.bs5().set());

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
