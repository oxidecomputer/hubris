// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use cortex_m_rt::pre_init;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[cfg(any(feature = "h743", feature = "h753"))]
#[pre_init]
unsafe fn system_pre_init() {
    // /!\ EXTREME DANGER WARNING /!\
    //
    // We are running this function *before* the startup routine has completed,
    // meaning that `static`s have NOT been initialized. This is extremely
    // likely to be unsound in the general case, and should probably be
    // rewritten in `global_asm!` some day, as the `pre_init` macro is now
    // deprecated.
    //
    // Until that day, you MUST NOT read or write any `static` variables, as
    // that would be IMMEDIATE Undefined Behavior. Tread carefully!
    //
    // /!\ EXTREME DANGER WARNING /!\
    //
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
    let pwr = unsafe { &*device::PWR::ptr() };
    // Poke CR3 to enable the LDO and prevent further writes.
    pwr.cr3.modify(|_, w| w.ldoen().set_bit());

    // Busy-wait until the ACTVOSRDY bit says that we've stabilized at VOS3.
    while !pwr.csr1.read().actvosrdy().bit() {
        // spin
    }

    // Turn on the internal RAMs.
    let rcc = unsafe { &*device::RCC::ptr() };
    rcc.ahb2enr.modify(|_, w| {
        w.sram1en()
            .set_bit()
            .sram2en()
            .set_bit()
            .sram3en()
            .set_bit()
    });

    // Okay, yay, we can use some RAMs now.

    #[cfg(any(feature = "h743", feature = "h753"))]
    {
        // Workaround for erratum 2.2.9 "Reading from AXI SRAM may lead to data
        // read corruption" - limits AXI SRAM read concurrency.
        let axi = unsafe { &*device::AXI::ptr() };
        axi.targ7_fn_mod
            .modify(|_, w| w.read_iss_override().set_bit());
    }

    // We'll do the rest in system_init.
}

pub struct ClockConfig {
    pub source: ClockSource,
    pub divm: u8,
    pub vcosel: device::rcc::pllcfgr::PLL1VCOSEL_A,
    pub pllrange: device::rcc::pllcfgr::PLL1RGE_A,
    pub divn: u16,
    pub divp: device::rcc::pll1divr::DIVP1_A,
    pub divq: u8,
    pub divr: u8,
    pub cpu_div: device::rcc::d1cfgr::D1CPRE_A,
    pub ahb_div: device::rcc::d1cfgr::HPRE_A,
    pub apb1_div: device::rcc::d2cfgr::D2PPRE1_A,
    pub apb2_div: device::rcc::d2cfgr::D2PPRE2_A,
    pub apb3_div: device::rcc::d1cfgr::D1PPRE_A,
    pub apb4_div: device::rcc::d3cfgr::D3PPRE_A,
    pub flash_latency: u8,
    pub flash_write_delay: u8,
}

pub enum ClockSource {
    ExternalCrystal,
    Hsi64,
}

pub fn system_init(config: ClockConfig) -> device::Peripherals {
    // Use the crate peripheral take mechanism to get peripherals.
    let cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    system_init_custom(cp, p, config)
}

pub fn system_init_custom(
    mut cp: cortex_m::Peripherals,
    p: device::Peripherals,
    config: ClockConfig,
) -> device::Peripherals {
    // Basic RAMs are working, power is stable, and the runtime has initialized
    // static variables.
    //
    // We are running at 64MHz on the HSI oscillator at voltage scale VOS3.
    //
    // Turn on CPU I/D caches to improve performance. This has a significant
    // impact on the delay loop a few lines below.
    cp.SCB.enable_icache();
    cp.SCB.enable_dcache(&mut cp.CPUID);

    // Before doing anything else, check for a measurement handoff token
    #[cfg(feature = "measurement-handoff")]
    unsafe {
        // After each delay, we'll wait roughly 200 ms.
        //
        // You might ask yourself, "how do we have a RETRY_COUNT if the closure
        // diverges"? Well! `measurement_handoff::check` stores the iteration
        // counter in a linker location that persists across soft-reboots.
        const DELAY_MICROS: u32 = 200 * 1_000;
        const RETRY_COUNT: u32 = 20;

        // APB1 is currently 64MHz. Create a rolling timer we can use for now.
        let timer = rolling_timer::RollingTimer::new_tim5(&p, 64);
        measurement_handoff::check(RETRY_COUNT, || {
            timer.blocking_delay_micros(DELAY_MICROS);
            cortex_m::peripheral::SCB::sys_reset()
        });
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
    // Halt I2C timeout clocks when the debugger halts the system.
    p.DBGMCU.apb1lfz1.modify(|_, w| {
        w.dbg_i2c1().set_bit();
        w.dbg_i2c2().set_bit();
        w.dbg_i2c3().set_bit();
        w
    });
    p.DBGMCU.apb4fz1.modify(|_, w| {
        w.dbg_i2c4().set_bit();
        w
    });

    // Set up SYSCFG selections so drivers don't have to.
    p.RCC.apb4enr.modify(|_, w| w.syscfgen().enabled());
    cortex_m::asm::dmb();

    // Ethernet is on RMII, not MII.
    p.SYSCFG.pmcr.modify(|_, w| unsafe { w.epis().bits(0b100) });

    // The Flash controller comes out of reset configured for 3 wait states.
    // That's approximately correct for 64MHz at VOS3, which is fortunate, since
    // we've been executing instructions out of flash _the whole time._

    // Our goal is now to boost the CPU frequency to its final level. This means
    // raising the core supply voltage from VOS3 and adding wait states or
    // reduced divisors to a bunch of things, and then finally making the actual
    // change. (The target state is VOS1 on the H743/53, and VOS0 on H7B3.)

    // We're allowed to hop directly from VOS3 to the target state; the manual
    // doesn't say this explicitly but the ST drivers do it.
    //
    // We want to set the same bits on both SoCs despite the naming differences.
    // On the H7B3, the register we're calling "D3CR" here is called "SRDCR" in
    // certain editions of the manual.
    p.PWR.d3cr.write(|w| unsafe { w.vos().bits(0b11) });
    // Busy-wait for the voltage to reach the right level.
    while !p.PWR.d3cr.read().vosrdy().bit() {
        // spin
    }
    // We are now at target voltage.

    match config.source {
        ClockSource::ExternalCrystal => {
            // There's an external crystal on the board. We'll use it as our
            // clock source, to get higher accuracy than the internal
            // oscillator. To do that we must turn on the High Speed External
            // oscillator.
            p.RCC.cr.modify(|_, w| w.hseon().set_bit());
            // Wait for it to stabilize.
            while !p.RCC.cr.read().hserdy().bit() {
                // spin
            }

            // The clock generator divides the external crystal frequency by
            // DIVM before feeding it to the VCO, and the result must be in the
            // range 2-16MHz.
            p.RCC
                .pllckselr
                .modify(|_, w| w.divm1().bits(config.divm).pllsrc().hse());

            // The VCO itself needs to be configured for the appropriate input
            // range and output range. We will also want its P-output, which is
            // the output that's tied to the system clock.
            //
            // We turn on the Q-output because it's used for a lot of peripheral
            // clocks, and the R-output for the trace unit.
            p.RCC.pllcfgr.modify(|_, w| {
                w.pll1vcosel()
                    .variant(config.vcosel)
                    .pll1rge()
                    .variant(config.pllrange)
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
            // must be in the range 192-836MHz. To avoid needing to configure
            // the fractional divider, we configure the VCO to 2x our target
            // frequency, 800MHz, which is in turn exactly 100x our (divided)
            // input frequency.
            //
            // The P value is the divisor from VCO frequency to system
            // frequency, so it needs to be 2 to get a 400MHz P-output.
            //
            // We set the R output to the same frequency because it's what
            // Humility currently expects, and drop the Q output for kernel
            // clock use.
            p.RCC.pll1divr.modify(|_, w| unsafe {
                w.divn1()
                    .bits(config.divn)
                    .divp1()
                    .variant(config.divp)
                    .divq1()
                    .bits(config.divq)
                    .divr1()
                    .bits(config.divr)
            });
        }
        ClockSource::Hsi64 => {
            p.RCC
                .pllckselr
                .write(|w| w.pllsrc().hsi().divm1().bits(config.divm));
            p.RCC.pllcfgr.write(|w| {
                w.pll1vcosel()
                    .variant(config.vcosel)
                    .pll1rge()
                    .variant(config.pllrange)
                    .divp1en()
                    .enabled()
                    .divr1en()
                    .enabled()
            });
            p.RCC.pll1divr.write(|w| unsafe {
                w.divp1()
                    .bits(config.divp as u8)
                    .divn1()
                    .bits(config.divn)
                    .divq1()
                    .bits(config.divq)
                    .divr1()
                    .bits(config.divr)
            });
        }
    }

    // Turn on PLL1 and wait for it to lock.
    p.RCC.cr.modify(|_, w| w.pll1on().on());
    while !p.RCC.cr.read().pll1rdy().bit() {
        // spin
    }

    // PLL1's frequency will become the system clock, which in turn goes through
    // a series of dividers to produce clocks for each system bus.
    // Configure peripheral clock dividers to make sure we stay within
    // range when we change oscillators.
    p.RCC.d1cfgr.write(|w| {
        w.d1cpre()
            .variant(config.cpu_div)
            .hpre()
            .variant(config.ahb_div)
            .d1ppre()
            .variant(config.apb3_div)
    });
    // Other APB buses at HCLK/2 = CPU/4 = 100MHz
    //
    // NOTE: until we call `configure_tim5` again below, our rolling timer will
    // be inaccurate as we have changed the prescaler (and will shortly be
    // changing the source clock fed to the prescaler!)
    p.RCC.d2cfgr.write(|w| {
        w.d2ppre1()
            .variant(config.apb1_div)
            .d2ppre2()
            .variant(config.apb2_div)
    });
    p.RCC.d3cfgr.write(|w| w.d3ppre().variant(config.apb4_div));

    // Flash must be configured with wait states and programming delays to
    // conform to the target speed; see ref man Table 13
    p.FLASH.acr.write(|w| unsafe {
        w.latency()
            .bits(config.flash_latency)
            .wrhighfreq()
            .bits(config.flash_write_delay)
    });
    loop {
        let r = p.FLASH.acr.read();
        if r.latency().bits() == config.flash_latency
            && r.wrhighfreq().bits() == config.flash_write_delay
        {
            break;
        }
    }
    // Not that reordering is likely here, since we polled, but: we
    // really do need the Flash to be programmed with more wait states
    // before switching the clock.
    cortex_m::asm::dmb();

    // Right! We're all set to change our clock without overclocking anything by
    // accident. Perform the switch.
    p.RCC.cfgr.write(|w| w.sw().pll1());
    while !p.RCC.cfgr.read().sws().is_pll1() {
        // spin
    }

    // set RNG clock to PLL1 clock
    #[cfg(any(feature = "h743", feature = "h753"))]
    p.RCC.d2ccip2r.modify(|_, w| w.rngsel().pll1_q());

    // Hand the peripherals back in case the board-specific setup code needs to
    // do anything.
    p
}

pub mod rolling_timer {
    use super::device;

    /// A 32-bit rolling hardware timer, ticking at 1MHz.
    pub struct RollingTimer<'a> {
        tim: &'a device::TIM5,
    }

    /// Stop the rolling timer automatically when dropped.
    impl Drop for RollingTimer<'_> {
        fn drop(&mut self) {
            self.tim.cr1.modify(|_r, w| w.cen().disabled());
        }
    }

    impl<'a> RollingTimer<'a> {
        /// Enable TIM5 for use as a 32-bit rolling timer at a tick rate of
        /// 1MHz.
        ///
        /// TIM5 will be enabled at the RCC level, and the current count value
        /// will be reset to zero. This function may be called multiple times,
        /// modulo the safety concerns listed below.
        ///
        /// `apb1_mhz` should be the configured frequency in MHz of the APB1
        /// clock, which is used as an input to TIM5, and will be used to
        /// pre-scale this input down to a tick rate of 1MHz.
        pub fn new_tim5(p: &'a device::Peripherals, apb1_mhz: u16) -> Self {
            // Hand-build TIM5 as a 32-bit rolling timer at 1 MHz. Start by
            // enabling TIM5 on APB1L in RCC and toggling reset
            p.RCC.apb1lenr.modify(|_r, w| w.tim5en().enabled());
            cortex_m::asm::dsb();

            p.RCC.apb1lrstr.modify(|_r, w| w.tim5rst().set_bit());
            p.RCC.apb1lrstr.modify(|_r, w| w.tim5rst().clear_bit());

            // Now, configure it for an upcounting rolling mode
            //
            // Disable counter
            p.TIM5.cr1.modify(|_r, w| w.cen().disabled());
            // Set auto-reload to u32::MAX
            p.TIM5.arr.write(|w| w.arr().bits(u32::MAX));
            // Set counter to zero
            p.TIM5.cnt.modify(|_r, w| w.cnt().bits(0));
            // Set prescaler to (FREQ / 1M) - 1, as the counter resets to 0
            // AFTER counting this number.
            p.TIM5.psc.write(|w| w.psc().bits(apb1_mhz - 1));
            // Generate update (latch the PSC and ARR values)
            p.TIM5.egr.write(|w| w.ug().set_bit());
            // Start counting!
            p.TIM5.cr1.modify(|_r, w| w.cen().enabled());

            Self { tim: &p.TIM5 }
        }

        /// Obtain the current count value of TIM5, which is a 32-bit timer that
        /// ticks at a rate of 1MHz.
        ///
        /// The value returned by this function "rolls over", or wraps around
        /// every 71 minutes or so. Callers should be careful to handle
        /// potential wrapping of the returned value when calculating elapsed
        /// time or using for delays.
        ///
        /// Consider using `blocking_delay_micros()`, which correctly handles
        /// this calculation, for early boot-up delays.
        ///
        /// NOTE: The returned value here is only valid while *this* instance of
        /// `RollingTimer` is valid. If the timer is dropped and recreated, the
        /// count will be reset to zero.
        #[inline(always)]
        pub fn get_rolling_micros(&self) -> u32 {
            self.tim.cnt.read().bits()
        }

        /// Perform a blocking delay for the given number of microseconds.
        #[inline]
        pub fn blocking_delay_micros(&self, micros: u32) {
            let start = self.get_rolling_micros();
            loop {
                let now = self.get_rolling_micros();

                // Since this is a rolling timer, we can perform a wrapping sub
                // to obtain the elapsed amount of time, even if we have crossed
                // the rollover point, e.g.:
                //
                // start  = 0xFFFF_FFFE
                // now    = 0x0000_0080
                //
                // now.wrapping_sub(start) => 0x82
                let elapsed = now.wrapping_sub(start);
                if elapsed >= micros {
                    break;
                }
            }
        }
    }
}
