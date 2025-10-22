// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

// System pre-init hook for establishing system properties required by Rust.
//
// This routine must run before anything touches RAM! The cortex-m-rt crate's
// Reset handler ensures this. As a result, we have to write this in raw
// assembly code, to avoid trying to push/pop a stack frame.
//
// Be very careful about reordering or removing things from this function.
core::arch::global_asm! {
    ".global __pre_init",
    ".type __pre_init_,%function",
    ".thumb_func",
    ".cfi_startproc",
    "__pre_init:",

    // PWR.CR3 has a write-once feature on the LDO enable bit. The processor
    // would like the power configuration to be stable before it guarantees that
    // writes to RAM will succeed (reference manual 6.4.1 "System supply
    // startup"). We're actually perfectly happy with the reset supply
    // configuration, which is VOS3 on the LDO. So, we'll write PWR.CR3 just to
    // lock it:
    "    movw r0, :lower16:{PWR_addr}",
    "    movt r0, :upper16:{PWR_addr}",
    "    ldr r1, [r0, #{PWR_CR3_offset}]",
    "    str r1, [r0, #{PWR_CR3_offset}]",

    // Technically we're supposed to ensure that we're stable at VOS3 before
    // continuing; this should already be ensured before our code was allowed to
    // run, but for safety's sake:
    "1:  ldr r1, [r0, #{PWR_CSR1_offset}]",
    "    tst r1, #(1 << {PWR_CSR1_ACTVOSRDY_bit})",
    "    beq 1b",

    // Turn on all of the smaller non-TCM non-AXI SRAMs, in case the program
    // puts data there.
    "    movw r0, :lower16:{RCC_addr}",
    "    movt r0, :upper16:{RCC_addr}",
    "    ldr r1, [r0, #{RCC_AHB2ENR_offset}]",
    "    orrs r1, #((1 << {RCC_AHB2ENR_SRAM1EN_bit}) \
                  | (1 << {RCC_AHB2ENR_SRAM2EN_bit}) \
                  | (1 << {RCC_AHB2ENR_SRAM3EN_bit}))",

    // Apply workaround for ST erratum 2.2.9 "Reading from AXI SRAM may lead to
    // data read corruption" - limits AXI SRAM read concurrency.
    "    movw r0, :lower16:{AXI_TARG7_FN_MOD_addr}",
    "    movt r0, :upper16:{AXI_TARG7_FN_MOD_addr}",
    "    ldr r1, [r0]",
    "    orrs r1, #(1 << {AXI_TARG7_FN_MOD_READ_ISS_OVERRIDE_bit})",
    "    str r1, [r0]",

    // Aaaaand we're done.
    "    bx lr",
    ".cfi_endproc",
    ".size __pre_init, . - __pre_init",

    PWR_addr = const 0x5802_4800, //device::PWR::ptr(),
    PWR_CSR1_offset = const 0x4, // reference manual 6.8.2
    PWR_CSR1_ACTVOSRDY_bit = const 13,
    PWR_CR3_offset = const 0xC, // reference manual 6.8.4

    RCC_addr = const 0x5802_4400, //device::RCC::ptr(),
    RCC_AHB2ENR_offset = const 0x0DC, // reference manual 8.7.42
    RCC_AHB2ENR_SRAM1EN_bit = const 29, // reference manual 8.7.42
    RCC_AHB2ENR_SRAM2EN_bit = const 30, // reference manual 8.7.42
    RCC_AHB2ENR_SRAM3EN_bit = const 31, // reference manual 8.7.42


    // The offset from the AXI block to this register is too large to do the
    // same base/displacement thing as the other peripherals above, so this
    // constant is the result of adding the base address from the reference
    // manual (0x5100_0000) to the offset from table 6.
    AXI_TARG7_FN_MOD_addr = const 0x5100_8108,
    AXI_TARG7_FN_MOD_READ_ISS_OVERRIDE_bit = const 0, // same
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
        const RETRY_COUNT: u32 = 20;
        measurement_handoff::check(RETRY_COUNT, || {
            cortex_m::asm::delay(12860000); // about 200 ms
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

    // Hello from target speed!

    // Hand the peripherals back in case the board-specific setup code needs to
    // do anything.
    p
}
