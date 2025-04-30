// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32h7;

use kern::profiling::EventsTable;
use stm32h7::stm32h753 as device;

use drv_stm32h7_startup::ClockConfig;

use cortex_m_rt::entry;

#[cfg(feature = "traptrace")]
mod tracing;

#[entry]
fn main() -> ! {
    let p = system_init();

    const CYCLES_PER_MS: u32 = 400_000;

    {
        p.RCC.ahb4enr.modify(|_, w| {
            w.gpiojen().set_bit();
            w
        });
        cortex_m::asm::dsb();
        p.GPIOJ.moder.modify(|_, w| {
            w.moder8().output();
            w.moder9().output();
            w.moder10().output();
            w.moder11().output();
            w.moder12().output();
            w.moder13().output();
            w
        });
    }
    kern::profiling::configure_events_table(&EVENTS);

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}

fn system_init() -> device::Peripherals {
    let cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    // Check the package we've been flashed on (Cosmo boards use BGA240)
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

    // Cosmo has resistors strapping three pins to indicate the board revision.
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
    // Assuming 50pF for the trace plus the FPGA's tristated input on the far
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
        if #[cfg(target_board = "cosmo-a")] {
            let expected_rev = 0b000;
        } else {
            compile_error!("not a recognized cosmo board")
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

    // Now that our clock tree is configured appropriately, we need to set up
    // the external bus to the FPGA. This ensures that we're not relying on any
    // particular task to do it, so we can direct-map its peripherals into other
    // tasks. If those tasks try to use the peripheral before the FPGA is
    // initialized, the peripherals won't work, but the result should not be
    // _fatal._

    // Pin mapping:
    // PB7      ADV_L (this is PB2 on the schematic, manually jumpered)
    //
    // PD0      DA2
    // PD1      DA3
    // PD2      -
    // PD3      CLK
    // PD4      OE_L
    // PD5      WE_L
    // PD6      WAIT_L  <-- pulled up in hardware
    // PD7      CS1_L
    // PD8      DA13
    // PD9      DA14
    // PD10     DA15
    // PD11     A16
    // PD12     A17
    // PD13     A18
    // PD14     DA0
    // PD15     DA1
    //
    // PE0      BL0_L
    // PE1      BL1_L
    // PE2      A23     <-- new
    // PE3      A19
    // PE4      A20     <-- new
    // PE5      A21     <-- new
    // PE6      A22     <-- new
    // PE7      DA4
    // PE8      DA5
    // PE9      DA6
    // PE10     DA7
    // PE11     DA8
    // PE12     DA9
    // PE13     DA10
    // PE14     DA11
    // PE15     DA12
    //
    // Our goal is to put all of these into the appropriate AF setting (which,
    // conveniently, is AF12 across all ports) and prepare the memory controller.

    // Ensure clock is enabled to both the GPIO ports we touch, and the FMC
    // itself.
    p.RCC.ahb3enr.modify(|_, w| w.fmcen().set_bit());
    p.RCC.ahb4enr.modify(|_, w| {
        w.gpioben().set_bit();
        w.gpioden().set_bit();
        w.gpioeen().set_bit();
        w
    });
    cortex_m::asm::dsb();

    // Expose all the pins _first._ This seems to work best.

    // GPIOB
    p.GPIOB.afrl.modify(|_, w| {
        w.afr7().af12();
        w
    });
    p.GPIOB.ospeedr.modify(|_, w| {
        w.ospeedr7().very_high_speed();
        w
    });
    p.GPIOB.moder.modify(|_, w| {
        w.moder7().alternate();
        w
    });

    // GPIOD
    p.GPIOD.afrl.modify(|_, w| {
        w.afr0().af12();
        w.afr1().af12();
        // pin 2 used for something else
        w.afr3().af12();
        w.afr4().af12();
        w.afr5().af12();
        w.afr6().af12();
        w.afr7().af12();
        w
    });
    p.GPIOD.afrh.modify(|_, w| {
        w.afr8().af12();
        w.afr9().af12();
        w.afr10().af12();
        w.afr11().af12();
        w.afr12().af12();
        w.afr13().af12();
        w.afr14().af12();
        w.afr15().af12();
        w
    });
    p.GPIOD.ospeedr.modify(|_, w| {
        w.ospeedr0().very_high_speed();
        w.ospeedr1().very_high_speed();
        // pin 2 used elsewhere
        w.ospeedr3().very_high_speed();
        w.ospeedr4().very_high_speed();
        w.ospeedr5().very_high_speed();
        w.ospeedr6().very_high_speed();
        w.ospeedr7().very_high_speed();

        w.ospeedr8().very_high_speed();
        w.ospeedr9().very_high_speed();
        w.ospeedr10().very_high_speed();
        w.ospeedr11().very_high_speed();
        w.ospeedr12().very_high_speed();
        w.ospeedr13().very_high_speed();
        w.ospeedr14().very_high_speed();
        w.ospeedr15().very_high_speed();
        w
    });
    p.GPIOD.moder.modify(|_, w| {
        w.moder0().alternate();
        w.moder1().alternate();
        // pin 2 used elsewhere
        w.moder3().alternate();
        w.moder4().alternate();
        w.moder5().alternate();
        w.moder6().alternate();
        w.moder7().alternate();

        w.moder8().alternate();
        w.moder9().alternate();
        w.moder10().alternate();
        w.moder11().alternate();
        w.moder12().alternate();
        w.moder13().alternate();
        w.moder14().alternate();
        w.moder15().alternate();
        w
    });

    // GPIOE
    // NOTE: this implementation only brings up a 20-bit address on the FMC bus,
    // because that's what worked on Grapefruit.  The hardware supports a 24-bit
    // address!
    p.GPIOE.afrl.modify(|_, w| {
        w.afr0().af12();
        w.afr1().af12();
        w.afr3().af12();
        w.afr7().af12();
        w
    });
    p.GPIOE.afrh.modify(|_, w| {
        w.afr8().af12();
        w.afr9().af12();
        w.afr10().af12();
        w.afr11().af12();
        w.afr12().af12();
        w.afr13().af12();
        w.afr14().af12();
        w.afr15().af12();
        w
    });
    p.GPIOE.ospeedr.modify(|_, w| {
        w.ospeedr0().very_high_speed();
        w.ospeedr1().very_high_speed();
        w.ospeedr3().very_high_speed();
        w.ospeedr7().very_high_speed();

        w.ospeedr8().very_high_speed();
        w.ospeedr9().very_high_speed();
        w.ospeedr10().very_high_speed();
        w.ospeedr11().very_high_speed();
        w.ospeedr12().very_high_speed();
        w.ospeedr13().very_high_speed();
        w.ospeedr14().very_high_speed();
        w.ospeedr15().very_high_speed();
        w
    });
    p.GPIOE.moder.modify(|_, w| {
        w.moder0().alternate();
        w.moder1().alternate();
        w.moder3().alternate();
        w.moder7().alternate();

        w.moder8().alternate();
        w.moder9().alternate();
        w.moder10().alternate();
        w.moder11().alternate();
        w.moder12().alternate();
        w.moder13().alternate();
        w.moder14().alternate();
        w.moder15().alternate();
        w
    });

    // Basic memory controller setup:
    p.FMC.bcr1.write(|w| {
        // Emit clock signal continuously, the FPGA likes that.
        w.cclken().set_bit();

        // Use synchronous bursts for both writes and reads.
        w.bursten().set_bit();
        w.cburstrw().set_bit();

        // Enable wait states.
        w.waiten().set_bit();
        // Expect the wait state to be active _during_ a wait, not one cycle
        // early.
        w.waitcfg().set_bit();

        // Enable writes through the controller.
        w.wren().set_bit();

        // Disable NOR flash memory access (may not be necessary?)
        w.faccen().clear_bit();

        // Configure the memory as "PSRAM" since that's the closest to the
        // behavior we want.
        //
        // Safety: this enum value is not modeled in the PAC, but is defined in
        // the reference manual, so this has no implications for safety.
        unsafe {
            w.mtyp().bits(0b01);
        }

        // Turn on the bank (note that we have not turned on the _controller_
        // still).
        w.mbken().set_bit();

        // The following fields are being deliberately left in their reset
        // states:
        // - FMCEN is being left off
        // - BMAP default (no remapping) is retained
        // - Write FIFO is being left on (TODO is this correct?)
        // - CPSIZE is being left with no special behavior on page-crossing
        // - ASYNCWAIT is being left off since we're synchronous
        // - EXTMOD is being left off, since it seems to only affect async
        // - WAITPOL is treating NWAIT as active low (could change if desired)
        // - MWID is being left at a 16 bit data bus.
        // - MUXEN is being left with a multiplexed A/D bus.

        w
    });

    // Now for the timings.
    //
    // Synchronous access write/read latency, minus 2. That is, 0 means 2 cycle
    // latency. Max value: 15 (for 17 cycles). NWAIT is not sampled until this
    // period has elapsed, so if you're handshaking with a device using NWAIT,
    // you almost certainly want this to be 0.
    const DATLAT: u8 = 0;
    // FMC_CLK division ratio relative to input (AHB3) clock, minus 1. Range:
    // 1..=15.
    //
    // Note from the clock config earlier in this function that AHB3 is running
    // at 200 MHz.
    const CLKDIV: u8 = 3; // /4, for 50 MHz -- field is divisor minus 1

    // Bus turnaround time in FMC_CLK cycles, 0..=15
    const BUSTURN: u8 = 0;

    p.FMC.btr1.write(|w| {
        unsafe {
            w.datlat().bits(DATLAT);
        }
        unsafe {
            w.clkdiv().bits(CLKDIV);
        }
        unsafe {
            w.busturn().bits(BUSTURN);
        }

        // Deliberately left in reset state and/or ignored:
        // - ACCMOD: only applies when EXTMOD is set in BCR above; also probably
        //   async only
        // - DATAST: async only
        // - ADDHLD: async only
        // - ADDSET: async only
        //
        w
    });

    // BWTR1 register is irrelevant if we're not using EXTMOD, which we're not,
    // currently.

    // Turn on the controller.
    p.FMC.bcr1.modify(|_, w| w.fmcen().set_bit());
    
    p
}

static EVENTS: kern::profiling::EventsTable = EventsTable {
    syscall_enter: |syscall_no| {
        pin_high(0);
    },
    syscall_exit: || {
        pin_low(0);
    },
    secondary_syscall_enter: ||(),
    secondary_syscall_exit: ||(),
    isr_enter: ||(),
    isr_exit: ||(),
    timer_isr_enter: ||(),
    timer_isr_exit: ||(),
    context_switch: |task_index| {
        let pin = match task_index {
            21 => {
                // spi2 driver
                Some(2)
            }
            15 => {
                // hash driver
                Some(3)
            }
            16 => {
                // spartan7 loader
                Some(4)
            }
            8 => {
                // hf
                Some(5)
            }
            12 => None,
            _ => Some(1),
        };
        for i in 0..6 {
            if pin == Some(i) {
                pin_high(i);
            } else {
                pin_low(i);
            }
        }
    },
};

fn map_pin(index: u8) -> u8 {
    match index {
        0..=1 => index,
        2..=6 => index + 1,
        _ => 14,
    }
}

fn pin_high(index: u8) {
    let gpio = unsafe { &*device::GPIOJ::ptr() };
    gpio.bsrr.write(|w| unsafe { w.bits(1 << map_pin(index)) });
}

fn pin_low(index: u8) {
    let gpio = unsafe { &*device::GPIOJ::ptr() };
    gpio.bsrr.write(|w| unsafe { w.bits(1 << (map_pin(index) + 16)) });
}
