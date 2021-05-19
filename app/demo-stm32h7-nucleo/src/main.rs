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

use cortex_m_rt::pre_init;

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32h7;

cfg_if::cfg_if! {
    if #[cfg(target_board = "stm32h7b3i-dk")] {
        use stm32h7::stm32h7b3 as device;
    } else if #[cfg(target_board = "nucleo-h743zi2")] {
        use stm32h7::stm32h743 as device;
    } else if #[cfg(target_board = "nucleo-h753zi")] {
        use stm32h7::stm32h753 as device;
    } else {
        compile_error!("target_board unknown or missing");
    }
}

use cortex_m_rt::entry;
use kern::app::App;

extern "C" {
    static hubris_app_table: App;
    static mut __sheap: u8;
    static __eheap: u8;
}

#[entry]
fn main() -> ! {
    system_init();

    cfg_if::cfg_if! {
        if #[cfg(target_board = "stm32h7b3i-dk")] {
            const CYCLES_PER_MS: u32 = 280_000;
        } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
            const CYCLES_PER_MS: u32 = 400_000;
        } else {
            compile_error!("target_board unknown or missing");
        }
    }

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

#[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))]
#[pre_init]
unsafe fn system_pre_init() {
    // Configure the power supply to latch the LDO on and prevent further
    // reconfiguration.
    //
    // Normally we would use Peripherals::take() to safely get a reference to
    // the PWR block, but that function expects RAM to be initialized and
    // writable. At this point, RAM is neither -- because the chip requires us
    // to get the power supply configuration right _before it guarantees that
    // RAM will work._
    //
    // Another case of the cortex_m/stm32 crates being designed with simpler
    // systems in mind.

    // Synthesize a pointer using a const fn (which won't hit RAM) and then
    // convert it to a reference. We can have a reference to PWR because it's
    // hardware, and is thus not uninitialized.
    let pwr = &*device::PWR::ptr();
    // Poke CR3 to enable the LDO and prevent further writes.
    pwr.cr3.modify(|_, w| w.ldoen().set_bit());

    // Busy-wait until the ACTVOSRDY bit says that we've stabilized at VOS3.
    while !pwr.csr1.read().actvosrdy().bit() {
        // spin
    }

    // Turn on the internal RAMs.
    let rcc = &*device::RCC::ptr();
    rcc.ahb2enr.modify(|_, w| {
        w.sram1en()
            .set_bit()
            .sram2en()
            .set_bit()
            .sram3en()
            .set_bit()
    });

    // Okay, yay, we can use some RAMs now.

    // We'll do the rest in system_init.
}

#[cfg(target_board = "stm32h7b3i-dk")]
#[pre_init]
unsafe fn system_pre_init() {
    // Configure our power supply to reflect how we're actually wired up on the
    // board. Specifically, we expect to run on the internal Switched-Mode Power
    // Supply (inexplicably called SD in the stm32h7 crate) without using the
    // internal LDO.
    //
    // Normally we would use Peripherals::take() to safely get a reference to
    // the PWR block, but that function expects RAM to be initialized and
    // writable. At this point, RAM is neither -- because the chip requires us
    // to get the power supply configuration right _before it guarantees that
    // RAM will work._
    //
    // Another case of the cortex_m/stm32 crates being designed with simpler
    // systems in mind.

    // Synthesize a pointer using a const fn (which won't hit RAM) and then
    // convert it to a reference. We can have a reference to PWR because it's
    // hardware, and is thus not uninitialized.
    let pwr = &*stm32h7::stm32h7b3::PWR::ptr();
    // Poke CR3 to enable SMPS (SD) and disable LDO.
    pwr.cr3.modify(|_, w| {
        w.sden()
            .set_bit()
            .ldoen()
            .clear_bit()
            .bypass()
            .clear_bit()
            .smpsexthp()
            .clear_bit()
    });

    // Busy-wait until the ACTVOSRDY bit says that we've stabilized at VOS3.
    while !pwr.csr1.read().actvosrdy().bit() {
        // spin
    }

    // Okay, yay, we can use some RAMs now.

    // We'll do the rest in system_init.
}

fn system_init() {
    // Basic RAMs are working, power is stable, and the runtime has initialized
    // static variables.
    //
    // We are running at 64MHz on the HSI oscillator at voltage scale VOS3.

    // Use the crate peripheral take mechanism to get peripherals.
    let mut cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    #[cfg(feature = "stm32h743")]
    {
        // Workaround for erratum 2.2.9 "Reading from AXI SRAM may lead to data
        // read corruption" - limits AXI SRAM read concurrency.
        p.AXI
            .targ7_fn_mod
            .modify(|_, w| w.read_iss_override().set_bit());
    }

    // The H7 -- and perhaps the Cortex-M7 -- has the somewhat annoying
    // property that any attempt to use ITM without having TRCENA set in
    // DBGMCU results in the FIFO never being ready (that is, ITM writes
    // spin).  This is not consistent with previous generations (e.g., M3,
    // M4), but it's also not inconsistent with the docs, which explicitly
    // warn that stimulus ports are in an undefined state if TRCENA hasn't
    // been set.  So we enable tracing on ourselves as a first action, even
    // though that isn't terribly meaningful if there is no debugger to
    // consume the ITM output.  It follows from the above, but just to be
    // unequivocal: ANY use of ITM prior to this point will lock the system
    // if/when an external debugger has not set TRCENA!
    cp.DCB.enable_trace();

    // Make sure debugging works in standby.
    p.DBGMCU.cr.modify(|_, w| {
        w.d3dbgcken()
            .set_bit()
            .d1dbgcken()
            .set_bit()
            .dbgstby_d1()
            .set_bit()
            .dbgstop_d1()
            .set_bit()
            .dbgsleep_d1()
            .set_bit()
    });

    // Set up SYSCFG selections so drivers don't have to.
    p.RCC.apb4enr.modify(|_, w| w.syscfgen().enabled());
    cortex_m::asm::dmb();

    // Ethernet is on RMII, not MII.
    p.SYSCFG.pmcr.modify(|_, w| unsafe { w.epis().bits(0b100) });

    // Turn on CPU I/D caches to improve performance at the higher clock speeds
    // we're about to enable.
    cp.SCB.enable_icache();
    cp.SCB.enable_dcache(&mut cp.CPUID);

    // The Flash controller comes out of reset configured for 3 wait states.
    // That's approximately correct for 64MHz at VOS3, which is fortunate, since
    // we've been executing instructions out of flash _the whole time._

    // Our goal is now to boost the CPU frequency to its final level. This means
    // raising the core supply voltage from VOS3 -- to VOS0 on H7B3, or VOS1 on
    // H743 -- and adding wait states or reduced divisors to a bunch of things,
    // and then finally making the actual change.

    // We're allowed to hop directly from VOS3 to VOS0/1; the manual doesn't say
    // this explicitly but the ST drivers do it.
    //
    // H7B3: The D3CR register is called SRDCR in the manual, and at the time of
    // this writing is incompletely modeled, so we have to unsafe the bits in.
    //
    // H743: Bits are still unsafe but name at least matches the manual. Note
    // that the H743 encodes VOS1 the same way the H7B3 encodes VOS0.
    p.PWR.d3cr.write(|w| unsafe { w.vos().bits(0b11) });
    // Busy-wait for the voltage to reach the right level.
    while !p.PWR.d3cr.read().vosrdy().bit() {
        // spin
    }
    // We are now at VOS1/0.

    // All configurations use PLL1. They just use it differently.

    cfg_if::cfg_if! {
        if #[cfg(target_board = "stm32h7b3i-dk")] {
            // There's a 24MHz crystal on our board. We'll use it as our clock
            // source, to get higher accuracy than the internal oscillator. Turn
            // on the High Speed External oscillator.
            p.RCC.cr.modify(|_, w| w.hseon().set_bit());
            // Wait for it to stabilize.
            while !p.RCC.cr.read().hserdy().bit() {
                // spin
            }

            // 24MHz HSE -> DIVM -> VCO input freq: the VCO's input must be in
            // the range 2-16MHz, so setting DIVM to divide by 3 gets us 8MHz.
            p.RCC
                .pllckselr
                .modify(|_, w| w.divm1().bits(3).pllsrc().hse());
            // The VCO itself needs to be configured to expect a 8MHz input
            // ("range 8") and at its normal (wide) range. We will also want its
            // P-output, which is the output that's tied to the system clock.
            //
            // We don't currently use the Q and R outputs, and we could switch
            // them off to save power -- but they can function as kernel clocks
            // for many of our peripherals, and thus might be useful.
            //
            // (Note that the R clock winds up being the source for the trace
            // unit.)
            p.RCC.pllcfgr.modify(|_, w| {
                w.pll1vcosel()
                    .wide_vco()
                    .pll1rge()
                    .range8()
                    .divp1en()
                    .enabled()
                    .divq1en()
                    .enabled()
                    .divr1en()
                    .enabled()
            });
            // Now, we configure the VCO for reals.
            //
            // The N value is the multiplication factor for the VCO internal
            // frequency relative to its input. The resulting internal frequency
            // must be in the range 128-560MHz. To avoid needing to configure
            // the fractional divider, we configure the VCO to 2x our target
            // frequency, 560MHz, which is in turn exactly 70x our (divided)
            // input frequency.
            //
            // The P value is the divisor from VCO frequency to system
            // frequency, so it needs to be 2 to get a 280MHz P-output.
            //
            // We set the Q and R outputs to the same frequency, because the
            // right choice isn't obvious yet.
            p.RCC.pll1divr.modify(|_, w| unsafe {
                w.divn1()
                    .bits(70 - 1)
                    .divp1()
                    .div2()
                    // Q and R fields aren't modeled correctly in the API, so:
                    .divq1()
                    .bits(1)
                    .divr1()
                    .bits(1)
            });
        } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
            // The H743 Nucleo board doesn't include an external crystal. Thus,
            // we use the HSI64 oscillator.

            // PLL1 configuration:
            // CPU freq = VCO / DIVP = HSI / DIVM * DIVN / DIVP
            //          = 64 / 4 * 50 / 2
            //          = 400 Mhz
            // System clock = 400 Mhz
            //  HPRE = /2  => AHB/Timer clock = 200 Mhz

            // Configure PLL
            let divm = 4;
            let divn = 50;
            let divp = 2;

            p.RCC.pllckselr.write(|w| {
                w.pllsrc().hsi()
                    .divm1().bits(divm)
            });
            p.RCC.pllcfgr.write(|w| {
                w.pll1vcosel().wide_vco()
                    .pll1rge().range8()
                    .divp1en().enabled()
                    .divr1en().enabled()
            });
            p.RCC.pll1divr.write(|w| unsafe {
                w.divp1().bits(divp - 1)
                    .divn1().bits(divn - 1)
                    .divr1().bits(divp - 1)
            });
        } else {
            compile_error!("target_board unknown or missing");
        }
    }

    // Turn on PLL1 and wait for it to lock.
    p.RCC.cr.modify(|_, w| w.pll1on().on());
    while !p.RCC.cr.read().pll1rdy().bit() {
        // spin
    }

    // PLL1's frequency will become the system clock, which in turn goes through
    // a series of dividers to produce clocks for each system bus.
    cfg_if::cfg_if! {
        if #[cfg(target_board = "stm32h7b3i-dk")] {
            // Delightfully, the 7B3 can run all of its buses at the same
            // frequency. So we can just set everything to 1.
            //
            // The clock domains appear to have been renamed after the SVD was
            // published. Here is the mapping.
            //
            // Manual   API
            // ------   ---
            // CD       D1, D2
            // SR       D3
            p.RCC
                .d1cfgr
                .write(|w| w.d1cpre().div1().d1ppre().div1().hpre().div1());
            p.RCC.d2cfgr.write(|w| w.d2ppre1().div1().d2ppre2().div1());
            p.RCC.d3cfgr.write(|w| w.d3ppre().div1());

            // Reconfigure the Flash wait states to support 280MHz operation at
            // VOS0.  Table 15 sez we need 6 wait states and 3 cycle programming
            // delay.
            p.FLASH
                .acr
                .write(|w| unsafe { w.latency().bits(6).wrhighfreq().bits(0b11) });
            // Because we're running from Flash, we really, really do need Flash
            // to have the right latency before moving on. Poll to see our
            // values get accepted and then barrier.
            while {
                let r = p.FLASH.acr.read();
                r.latency().bits() != 6 || r.wrhighfreq().bits() != 0b11
            } {
                // spin
            }
            cortex_m::asm::dmb();
        } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
            // Configure peripheral clock dividers to make sure we stay within
            // range when we change oscillators.  At VOS1, the AHB frequency is
            // limited to 200Mhz and the APB frequency is limited to 100MHz
            // (from Table 122 in the datasheet).
            p.RCC.d1cfgr.write(|w| {
                w.d1cpre().div1() // CPU at full rate
                    .hpre().div2()  // AHB at half that (200MHz)
                    .d1ppre().div2()    // D1 APB3 at half that again (100MHz)
            });

            // Clamp our APBs at 100MHz
            p.RCC.d2cfgr.write(|w| w.d2ppre1().div2().d2ppre2().div2());
            p.RCC.d3cfgr.write(|w| w.d3ppre().div2());

            // Configure Flash for 200MHz (AHB) at VOS1: 2WS, 2 programming
            // delay. See ref man Table 13
            p.FLASH.acr.write(|w| unsafe { w.latency().bits(2).wrhighfreq().bits(2) });
            while {
                let r = p.FLASH.acr.read();
                r.latency().bits() != 2 || r.wrhighfreq().bits() != 2
            } {}
            // Not that reordering is likely here, since we polled, but: we
            // really do need the Flash to be programmed with more wait states
            // before switching the clock.
            cortex_m::asm::dmb();
        } else {
            compile_error!("target_board unknown or missing");
        }
    }

    // Right! We're all set to change our clock without overclocking anything by
    // accident. Perform the switch.
    p.RCC.cfgr.write(|w| w.sw().pll1());
    while !p.RCC.cfgr.read().sws().is_pll1() {
        // spin
    }

    // Hello from 280MHz/400MHz/whatever!

    cfg_if::cfg_if! {
        if #[cfg(target_board = "stm32h7b3i-dk")] {
            // Finally, turn off the HSI we used at boot, to save about 400 uA.
            p.RCC.cr.modify(|_, w| w.hsion().off());
            // No need to busy wait here, the moment when it turns off is not
            // important.
            initialize_sdram(&mut cp, &p);
        }
    }
}

/// Sets up the SDRAM on the STM32H7B3I-DK board.
///
/// Note that this function requires a `Peripherals`, meaning it cannot be
/// executed during `pre_init`. This implies that the kernel (and any code
/// around `main`) _cannot_ store any information in SDRAM. Tasks, however, are
/// free to use it as they see fit.
#[cfg(target_board = "stm32h7b3i-dk")]
fn initialize_sdram(
    cp: &mut cortex_m::Peripherals,
    p: &stm32h7::stm32h7b3::Peripherals,
) {
    // Time to get the SDRAM turned on!
    //
    // The STM32H7B3I-DK board has a single SDRAM chip, an ISSI
    // IS42S16800F-6BLI. That's a 16-bit by 2Mi by 4 bank part in the 6ns-or-so
    // speed grade. It's attached to SDRAM controller bank 2 on the chip (not to
    // be confused with the chip's _internal_ banks); controller bank 1 is
    // unused.
    //
    // At CAS latency 3 this chip can run at up to 166MHz, but it appears that
    // the H7B3's SDRAM interface is limited to 110MHz. The nearest multiple we
    // can run it at is 280MHz/3, or 93.3333...Mhz.

    // Turn on the memory controller's clock. We're going to tap the AHB3 clock,
    // which is 280MHz, which will work nicely.
    p.RCC.cdccipr.modify(|_, w| w.fmcsel().rcc_hclk3());
    p.RCC.ahb3enr.modify(|_, w| w.fmcen().enabled());
    cortex_m::asm::dmb();

    // Switch the GPIOs we need. This is kind of a lot of code so it's pulled
    // out into a separate function.
    initialize_sdram_pins(&p);

    // Configure basic device features. Some of these apply across both banks
    // and must be written into controller bank 1's registers, even though we're
    // not otherwise using controller bank 1.
    p.FMC.sdbank1().sdcr.write(|w| unsafe {
        w.rpipe()
            .bits(0b10) // +2 cycles
            .rburst()
            .set_bit() // use burst mode, kind of
            .sdclk()
            .bits(0b11) // 1/3 input clock rate (93.33MHz)
    });
    p.FMC.sdbank2().sdcr.write(|w| unsafe {
        w.wp()
            .clear_bit() // don't write protect
            .cas()
            .bits(0b11) // CAS=3
            .nb()
            .set_bit() // 4 banks
            .mwid()
            .bits(0b01) // 16 data bits
            .nr()
            .bits(0b01) // 12 row address bits
            .nc()
            .bits(0b01) // 9 column address bits
    });

    // Set up timing. Again, some of these settings are global and must be
    // written to bank 1.
    p.FMC.sdbank1().sdtr.write(|w| unsafe {
        w.trp()
            .bits(3 - 1) // Trp = command period, PRE -> ACT
            .trc()
            .bits(10 - 1) // Trc = command period ACT -> ACT
    });
    p.FMC.sdbank2().sdtr.write(|w| unsafe {
        w.trcd()
            .bits(3 - 1) // Trcd = ACT to R/W delay
            .twr()
            .bits(4 - 1) // Twr is ST-specific, Tras - Trcd
            .tras()
            .bits(7 - 1) // Tras = command period ACT -> PRE
            .txsr()
            .bits(10 - 1) // Txsr = exit self refresh -> ACT
            .tmrd()
            .bits(2 - 1) // Tmrd = mode register program time
    });

    // Turn on the memory controller. Where does it say to do this in the SDRAM
    // docs? Nowhere! It's actually difficult to even find the definition of
    // this register! Whee!
    p.FMC.bcr1.modify(|_, w| w.fmcen().set_bit());

    // Start a Clock Configuration Enable (001) command to controller bank 2.
    // This will start clocking the RAM.
    p.FMC
        .sdcmr
        .write(|w| unsafe { w.mode().bits(0b001).ctb2().set_bit() });

    // The RAM needs 100us after clock is applied to wake up.
    early_delay(&mut cp.SYST, 280 * 100);

    // Start a PALL (All Bank Precharge) command on controller bank 2.
    p.FMC
        .sdcmr
        .write(|w| unsafe { w.mode().bits(0b010).ctb2().set_bit() });

    // Start an Auto-Refresh command on controller bank 2. The controller will
    // automatically repeat this command for the count given at NRFS. Our RAM
    // doesn't appear to specify this, but the ST docs suggest 8, so... 8!
    p.FMC.sdcmr.write(|w| unsafe {
        w.mode().bits(0b011).ctb2().set_bit().nrfs().bits(8 - 1) // ??
    });

    // Start a Load Mode Register command, loading the MRD field into the
    // SDRAM's config register.
    p.FMC.sdcmr.write(|w| unsafe {
        let mrd = 0b000 << 0 // burst length 1
            | 0b0 << 3 // burst type sequential
            | 0b11 << 4 // CAS latency 3
            | 1 << 9 // single location burst
            ;
        w.mode().bits(0b100).ctb2().set_bit().mrd().bits(mrd)
    });

    // Configure the refresh rate timer. This controls the reload value of a
    // countdown timer; a refresh is activated when it reaches zero.
    p.FMC.sdrtr.write(|w| unsafe {
        let refresh_ms: u32 = 64; // from datasheet
        let cycles_per_ms = 93_333; // for 93.333MHz SDCLK rate
        let refresh_cycles = refresh_ms * cycles_per_ms;
        let refresh_cyc_per_row = refresh_cycles / 4096; // round down
        let margin = 20; // from STM32H7B3 manual

        assert!(refresh_cyc_per_row < (1 << 13) - 1);
        w.cre()
            .set_bit()
            .count()
            .bits(refresh_cyc_per_row as u16 - margin)
    });
    cortex_m::asm::dmb();

    // All done.
}

/// Delays for at least `cycles` CPU cycles, using the sys tick timer.
///
/// This assumes the systick is not otherwise used, and freely overwrites its
/// configuration. Thus, this function is not safe to use after the kernel
/// starts -- but it's kosher during system init.
#[cfg(target_board = "stm32h7b3i-dk")]
fn early_delay(syst: &mut cortex_m::peripheral::SYST, cycles: u32) {
    assert!(cycles < 1 << 16);
    unsafe {
        syst.rvr.write(cycles - 1);
        syst.cvr.write(0);
        syst.csr.modify(|v| v | 0b101);
        while syst.csr.read() & (1 << 16) == 0 {
            // spin
        }
        syst.csr.write(0);
    }
}

/// Handy macro for expressing a word with particular bits set.
///
/// `bits!(0, 16, 17) == 0x3001`
#[cfg(target_board = "stm32h7b3i-dk")]
macro_rules! bits {
    ($($bit:expr),*) => { 0 $(| (1 << $bit))* };
}

#[cfg(target_board = "stm32h7b3i-dk")]
fn initialize_sdram_pins(p: &stm32h7::stm32h7b3::Peripherals) {
    p.RCC.ahb4enr.modify(|_, w| {
        w.gpioden()
            .enabled()
            .gpioeen()
            .enabled()
            .gpiofen()
            .enabled()
            .gpiogen()
            .enabled()
            .gpiohen()
            .enabled()
    });
    cortex_m::asm::dmb();

    // PD0  = FMC_D2
    // PD1  = FMC_D3
    // PD8  = FMC_D13
    // PD9  = FMC_D14
    // PD10 = FMC_D15
    // PD14 = FMC_D0
    // PD15 = FMC_D1
    configure_several_sdram_pins(&p.GPIOD, bits!(0, 1, 8, 9, 10, 14, 15));

    // PE0  = FMC_NBL0
    // PE1  = FMC_NBL1
    // PE7  = FMC_D4
    // PE8  = FMC_D5
    // PE9  = FMC_D6
    // PE10 = FMC_D7
    // PE11 = FMC_D8
    // PE12 = FMC_D9
    // PE13 = FMC_D10
    // PE14 = FMC_D11
    // PE15 = FMC_D12
    configure_several_sdram_pins(
        &p.GPIOE,
        bits!(0, 1, 7, 8, 9, 10, 11, 12, 13, 14, 15),
    );

    // PF0  = FMC_A0
    // PF1  = FMC_A1
    // PF2  = FMC_A2
    // PF3  = FMC_A3
    // PF4  = FMC_A4
    // PF5  = FMC_A5
    // PF11 = FMC_SDNRAS
    // PF12 = FMC_A6
    // PF13 = FMC_A7
    // PF14 = FMC_A8
    // PF15 = FMC_A9
    configure_several_sdram_pins(
        &p.GPIOF,
        bits!(0, 1, 2, 3, 4, 5, 11, 12, 13, 14, 15),
    );

    // PG0  = FMC_A10
    // PG1  = FMC_A11
    // PG4  = FMC_BA0
    // PG5  = FMC_BA1
    // PG8  = FMC_SDCLK
    // PG15 = FMC_SDNCAS
    configure_several_sdram_pins(&p.GPIOG, bits!(0, 1, 4, 5, 8, 15));

    // PH5 = FMC_SDNWE
    // PH6 = FMC_SDNE1
    // PH7 = FMC_SDCKE1
    configure_several_sdram_pins(&p.GPIOH, bits!(5, 6, 7));
}

/// Performs a sequence of 5 register writes to configure 0-16 GPIO pins in a
/// single port for SDRAM usage. By referencing the pins as a mask, we can avoid
/// the need for a port-specific sequence of `.moder0().yes().moder1().yes()`
/// nonsense.
///
/// This could be made general if you need it for something.
///
/// Note that `port` is GPIOA because, in the future, all GPIO ports are GPIOA.
#[cfg(target_board = "stm32h7b3i-dk")]
fn configure_several_sdram_pins(
    port: &impl core::ops::Deref<Target = stm32h7::stm32h7b3::gpioa::RegisterBlock>,
    mask: u16,
) {
    // If you wanted to make this general, change these constants into
    // arguments.
    const MODER_ALTERNATE: u32 = 0b10;
    const OTYPER_PUSH_PULL: u32 = 0b0;
    const OSPEEDR_LUDICROUS: u32 = 0b11;
    const AFR_AF12: u32 = 12;

    // The GPIO config registers come in 1, 2, and 4-bit per field variants. The
    // user-submitted mask is already correct for the 1-bit fields; we need to
    // expand it into corresponding 2- and 4-bit masks. We use an outer perfect
    // shuffle operation for this, which interleaves zeroes from the top 16 bits
    // into the bottom 16.

    // 1 in each targeted 1bit field.
    let mask_1 = u32::from(mask);
    // 0b01 in each targeted 2bit field.
    let lsbs_2 = outer_perfect_shuffle(mask_1);
    // 0b0001 in each targeted 4bit field for low half.
    let lsbs_4l = outer_perfect_shuffle(lsbs_2 & 0xFFFF);
    // Same for high half.
    let lsbs_4h = outer_perfect_shuffle(lsbs_2 >> 16);

    // Corresponding masks, with 1s in all field bits instead of just the LSB:
    let mask_2 = lsbs_2 * 0b11;
    let mask_4l = lsbs_4l * 0b1111;
    let mask_4h = lsbs_4h * 0b1111;

    // MODER contains 16x 2-bit fields.
    port.moder.write(|w| unsafe {
        w.bits(
            (port.moder.read().bits() & !mask_2) | (MODER_ALTERNATE * lsbs_2),
        )
    });
    // OTYPER contains 16x 1-bit fields.
    port.otyper.write(|w| unsafe {
        w.bits(
            (port.otyper.read().bits() & !mask_1) | (OTYPER_PUSH_PULL * mask_1),
        )
    });
    // OSPEEDR contains 16x 2-bit fields.
    port.ospeedr.write(|w| unsafe {
        w.bits(
            (port.ospeedr.read().bits() & !mask_2)
                | (OSPEEDR_LUDICROUS * lsbs_2),
        )
    });
    // AFRx contains 8x 4-bit fields.
    port.afrl.write(|w| unsafe {
        w.bits((port.afrl.read().bits() & !mask_4l) | (AFR_AF12 * lsbs_4l))
    });
    port.afrh.write(|w| unsafe {
        w.bits((port.afrh.read().bits() & !mask_4h) | (AFR_AF12 * lsbs_4h))
    });
}

/// Interleaves bits in `input` as follows:
///
/// - Output bit 0 = input bit 0
/// - Output bit 1 = input bit 15
/// - Output bit 2 = input bit 1
/// - Output bit 3 = input bit 16
/// ...and so forth.
///
/// This is a great example of one of those bit twiddling tricks you never
/// expected to need. Method from Hacker's Delight.
///
/// In practice, this compiles to zero instructions, because we use it with
/// constant operands (note the `const fn` part).
#[cfg(target_board = "stm32h7b3i-dk")]
const fn outer_perfect_shuffle(mut input: u32) -> u32 {
    let mut tmp = (input ^ (input >> 8)) & 0x0000ff00;
    input ^= tmp ^ (tmp << 8);
    tmp = (input ^ (input >> 4)) & 0x00f000f0;
    input ^= tmp ^ (tmp << 4);
    tmp = (input ^ (input >> 2)) & 0x0c0c0c0c;
    input ^= tmp ^ (tmp << 2);
    tmp = (input ^ (input >> 1)) & 0x22222222;
    input ^= tmp ^ (tmp << 1);
    input
}
