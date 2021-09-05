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

use stm32h7::stm32h743 as device;

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

    // Okay, yay, we can use some RAMs now.

    // We'll do the rest in system_init.
}

const USE_EXTERNAL_CRYSTAL: bool = true;

fn system_init() {
    // Basic RAMs are working, power is stable, and the runtime has initialized
    // static variables.
    //
    // We are running at 64MHz on the HSI oscillator at voltage scale VOS3.

    // Use the crate peripheral take mechanism to get peripherals.
    let mut cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    // Workaround for erratum 2.2.9 "Reading from AXI SRAM may lead to data
    // read corruption" - limits AXI SRAM read concurrency.
    p.AXI
        .targ7_fn_mod
        .modify(|_, w| w.read_iss_override().set_bit());

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

    // Turn on CPU I/D caches to improve performance at the higher clock speeds
    // we're about to enable.
    cp.SCB.enable_icache();
    cp.SCB.enable_dcache(&mut cp.CPUID);

    // The Flash controller comes out of reset configured for 3 wait states.
    // That's approximately correct for 64MHz at VOS3, which is fortunate, since
    // we've been executing instructions out of flash _the whole time._

    // Our goal is now to boost the CPU frequency to its final level. This means
    // raising the core supply voltage from VOS3 -- to VOS1 on H753 -- and
    // adding wait states or reduced divisors to a bunch of things, and then
    // finally making the actual change.

    // We're allowed to hop directly from VOS3 to VOS1; the manual doesn't say
    // this explicitly but the ST drivers do it.
    //
    // Bits are still unsafe in the API but name at least matches the manual.
    p.PWR.d3cr.write(|w| unsafe { w.vos().bits(0b11) });
    // Busy-wait for the voltage to reach the right level.
    while !p.PWR.d3cr.read().vosrdy().bit() {
        // spin
    }
    // We are now at VOS1/0.

    if USE_EXTERNAL_CRYSTAL {
        // There's an 8MHz crystal on our board. We'll use it as our clock
        // source, to get higher accuracy than the internal oscillator. Turn
        // on the High Speed External oscillator.
        p.RCC.cr.modify(|_, w| w.hseon().set_bit());
        // Wait for it to stabilize.
        while !p.RCC.cr.read().hserdy().bit() {
            // spin
        }

        // 8MHz HSE -> DIVM -> VCO input freq: the VCO's input must be in the
        // range 2-16MHz, so we want to bypass the prescaler by setting DIVM to
        // 1.
        p.RCC
            .pllckselr
            .modify(|_, w| w.divm1().bits(1).pllsrc().hse());
        // The VCO itself needs to be configured to expect a 8MHz input
        // ("wide input range" or, on the slightly later parts, "range 8") and
        // at its normal (wide) output range. We will also want its P-output,
        // which is the output that's tied to the system clock.
        //
        // The Q tap goes to a bunch of peripheral kernel clocks. The R clock
        // goes to the trace unit.
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
        // must be in the range 192-836MHz. To avoid needing to configure
        // the fractional divider, we configure the VCO to 2x our target
        // frequency, 800MHz, which is in turn exactly 100x our (divided)
        // input frequency.
        //
        // The P value is the divisor from VCO frequency to system
        // frequency, so it needs to be 2 to get a 400MHz P-output.
        //
        // We set the R output to the same frequency because it's what Humility
        // currently expects, and drop the Q output for kernel clock use.
        p.RCC.pll1divr.modify(|_, w| unsafe {
            w.divn1()
                .bits(100 - 1)
                .divp1()
                .div2()
                // Q and R fields aren't modeled correctly in the API, so:
                .divq1()
                .bits(4 - 1)
                .divr1()
                .bits(1)
        });
    } else {
        // This clock setup code is based on the H743 Nucleo code, which didn't
        // include an external crystal -- so it uses HSI64. (TODO: fix for
        // actual Gemini crystal on HSE.)

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

        p.RCC
            .pllckselr
            .write(|w| w.pllsrc().hsi().divm1().bits(divm));
        p.RCC.pllcfgr.write(|w| {
            w.pll1vcosel()
                .wide_vco()
                .pll1rge()
                .range8()
                .divp1en()
                .enabled()
                .divr1en()
                .enabled()
        });
        p.RCC.pll1divr.write(|w| unsafe {
            w.divp1()
                .bits(divp - 1)
                .divn1()
                .bits(divn - 1)
                .divr1()
                .bits(divp - 1)
        });
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
            .div1() // CPU at full rate
            .hpre()
            .div2() // AHB at half that (200mhz)
            .d1ppre()
            .div2() // D1 APB3 a further 1/2 down (100mhz)
    });
    // Other APB buses at HCLK/2 = CPU/4 = 100MHz
    p.RCC.d2cfgr.write(|w| w.d2ppre1().div2().d2ppre2().div2());
    p.RCC.d3cfgr.write(|w| w.d3ppre().div2());

    // Configure Flash for 200MHz (AHB) at VOS1: 2WS, 2 programming
    // delay. See ref man Table 13
    p.FLASH
        .acr
        .write(|w| unsafe { w.latency().bits(2).wrhighfreq().bits(2) });
    while {
        let r = p.FLASH.acr.read();
        r.latency().bits() != 2 || r.wrhighfreq().bits() != 2
    } {}
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

    // Hello from 400MHz!
}
