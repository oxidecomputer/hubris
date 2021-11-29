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

use cortex_m_rt::entry;
use kern::app::App;
use lpc55_pac as device;

extern "C" {
    static hubris_app_table: App;
    static mut __sheap: u8;
    static __eheap: u8;
}

#[cfg(feature = "plls")]
fn setup_clocks() {
    // From the manual:
    //
    // The clock to the SYSCON block is
    // always enabled. By default, the SYSCON block is clocked by the
    // FRO 12 MHz (fro_12m).
    //
    // LPC55 I2C driver has a note about weird crashes with the 150MHz PLL
    // and i2c?

    let syscon = unsafe { &*device::SYSCON::ptr() };
    let anactrl = unsafe { &*device::ANACTRL::ptr() };
    let pmc = unsafe { &*device::PMC::ptr() };

    // apparently some of the clocks are controlled by the analog block
    //
    anactrl
        .fro192m_ctrl
        .modify(|_, w| w.ena_96mhzclk().enable());
    anactrl
        .fro192m_ctrl
        .modify(|_, w| w.ena_12mhzclk().enable());

    // Enable the 1Mhz FRO for utick module
    syscon.clock_ctrl.modify(|_, w| {
        w.fro1mhz_clk_ena().enable().fro1mhz_utick_ena().enable()
    });

    // Use the FR0 12MHz clock for the main clock to start
    // We'll be switching over to the PLL later
    syscon.mainclksela.modify(|_, w| w.sel().enum_0x0());
    // Use Main clock A
    syscon.mainclkselb.modify(|_, w| w.sel().enum_0x0());

    // Divide the AHB clk by 1
    syscon.ahbclkdiv.modify(|_, w| unsafe { w.div().bits(0x0) });
    // 2 system clocks flash access time
    syscon.fmccr.modify(|_, w| unsafe { w.flashtim().bits(1) });

    // Some PLL math: Per 4.6.6.3.1 in the manual:
    //
    //  F_out = F_cco / (2 * P) = F_in * M / (N * 2 * P)
    //
    //  or written out in a nicer way
    //
    //  F_out * N * P * 2 = M * F_in
    //
    //  With a note that F_cco has to be > 275 Mhz. We want
    //  the maximum 150 Mhz. There are multiple solutions to
    //  this equation but the ones which seem to be stable are
    //  F_in = 12 Mhz
    //  N = 8
    //  P = 1
    //  M = 200
    //
    let pll_n = 8;
    let pll_p = 1;
    let pll_m = 200;

    // From 4.6.6.3.2 we need to calculate the bandwidth
    //
    // selp = floor(M/4) + 1
    //
    // selp = floor(200/4) + 1
    //
    // selp = 50 + 1 which gets rounded down to 31
    //
    // if (M >= 8000) => seli = 1
    // if (8000 > M >= 122) => seli = floor(8000/M)
    // if (122 > M >= 1) => seli = 2 * floor(M/4) + 3
    //
    // seli = floor(8000/M)
    //
    // seli = 40
    //
    // For normal applications the value for selr[3:0] must be kept 0.
    //
    let selp = 31;
    let seli = 40;
    let selr = 0;

    // Make sure these are actually off
    pmc.pdruncfg0.modify(|_, w| {
        w.pden_pll0().poweredoff().pden_pll0_sscg().poweredoff()
    });

    // Mark PLL0 as using 12 MHz
    syscon.pll0clksel.modify(|_, w| w.sel().enum_0x0());

    syscon.pll0ctrl.modify(|_, w| unsafe {
        w.selr()
            .bits(selr)
            .seli()
            .bits(seli)
            .selp()
            .bits(selp)
            .clken()
            .enable()
    });

    // writing these settings is 'quirky'. We have to write the
    // value once into the register then write it again with the latch
    // bit set. Not in the docs but in the NXP C driver...
    syscon.pll0ndec.write(|w| unsafe { w.ndiv().bits(pll_n) });
    syscon
        .pll0ndec
        .write(|w| unsafe { w.ndiv().bits(pll_n).nreq().set_bit() });

    syscon.pll0pdec.write(|w| unsafe { w.pdiv().bits(pll_p) });
    syscon
        .pll0pdec
        .write(|w| unsafe { w.pdiv().bits(pll_p).preq().set_bit() });

    syscon
        .pll0sscg1
        .write(|w| unsafe { w.mdiv_ext().bits(pll_m).sel_ext().set_bit() });
    syscon.pll0sscg1.write(|w| unsafe {
        w.mdiv_ext()
            .bits(pll_m)
            .sel_ext()
            .set_bit()
            .mreq()
            .set_bit()
            .md_req()
            .set_bit()
    });

    // Now actually turn on the PLLs
    pmc.pdruncfg0
        .modify(|_, w| w.pden_pll0().poweredon().pden_pll0_sscg().poweredon());

    // Time to put the Lock in Phase Locked Loop!
    //
    // 4.6.6.5.2 The start-up time is 500 μs + 300 / Fref seconds
    // The NXP C driver (i.e. documentation) just uses 6 ms for everything
    // even if we need a significantly lower start up time with Fref
    // of 12 mhz based on that formula. We can check this out later.
    //
    // Now of course comes the question of how we delay. The
    // cortex-m crate has a delay function to delay for at least
    // n instruction cycles. We're running at a maximum 150 mhz.
    //
    // n ins = 6 msec * 150 000 000 ins / sec * 1 sec / 1 000 msec
    //
    //  = 900000 instructions
    //
    // ...which is certainly an upper bound that can be adjusted
    //
    cortex_m::asm::delay(900000);

    // The flash wait cycles need to be adjusted. Per the docs
    // 0xb = 12 system clocks flash access time (for system clock rates up
    // to 150 MHz).
    syscon
        .fmccr
        .modify(|_, w| unsafe { w.flashtim().bits(0xb) });

    // Now actually set our clocks
    // Main A = 12 MHz
    syscon.mainclksela.modify(|_, w| w.sel().enum_0x0());
    // Main B = PLL0
    syscon.mainclkselb.modify(|_, w| w.sel().enum_0x1());
}

#[entry]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(feature = "plls")] {
            setup_clocks();

            const CYCLES_PER_MS: u32 = 150_000;
        } else {
            const CYCLES_PER_MS: u32 = 96_000;
        }
    }

    unsafe {
        //
        // To allow for SWO (the vector for ITM output), we must explicitly
        // enable it on pin0_10.
        //
        let iocon = &*device::IOCON::ptr();
        iocon.pio0_10.modify(|_, w| w.func().alt6());

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
