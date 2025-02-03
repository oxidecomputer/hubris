// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// We have to do this if we don't otherwise use it to ensure its vector table
// gets linked in.
extern crate stm32h7;

cfg_if::cfg_if! {
    if #[cfg(target_board = "nucleo-h743zi2")] {
        use stm32h7::stm32h743 as device;
    } else if #[cfg(target_board = "nucleo-h753zi")] {
        use stm32h7::stm32h753 as device;
    } else {
        compile_error!("target_board unknown or missing");
    }
}

use cortex_m_rt::entry;
use drv_stm32h7_startup::{system_init, ClockConfig};

#[entry]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
            const CYCLES_PER_MS: u32 = 400_000;
            const CLOCKS: ClockConfig = ClockConfig {
                // The Nucleo board doesn't include an external crystal, so we
                // derive clocks from the HSI64 oscillator.
                source: drv_stm32h7_startup::ClockSource::Hsi64,
                // 64MHz oscillator frequency is outside VCO input range of
                // 2-16, so we use DIVM to divide it by 4 to 16MHz.
                divm: 4,
                // This means the VCO must accept its wider input range:
                vcosel: device::rcc::pllcfgr::PLL1VCOSEL_A::WIDEVCO,
                pllrange: device::rcc::pllcfgr::PLL1RGE_A::RANGE8,
                // DIVN governs the multiplication of the VCO input frequency to
                // produce the intermediate frequency. We want an IF of 800MHz,
                // or a multiplication of 50x.
                //
                // We subtract 1 to get the DIVN value because the PLL
                // effectively adds one to what we write.
                divn: 50 - 1,
                // P is the divisor from the VCO IF to the system frequency. We
                // want 400MHz, so:
                divp: device::rcc::pll1divr::DIVP1_A::DIV2,
                // Q produces kernel clocks; we set it to 200MHz:
                divq: 4 - 1,
                // R is mostly used by the trace unit and we leave it fast:
                divr: 2 - 1,

                // We run the CPU at the full core rate of 400MHz:
                cpu_div: device::rcc::d1cfgr::D1CPRE_A::DIV1,
                // We down-shift the AHB by a factor of 2, to 200MHz, to meet
                // its constraints (Table 122 in datasheet)
                ahb_div: device::rcc::d1cfgr::HPRE_A::DIV2,
                // We configure all APB for 100MHz. These are relative to the
                // AHB frequency.
                apb1_div: device::rcc::d2cfgr::D2PPRE1_A::DIV2,
                apb2_div: device::rcc::d2cfgr::D2PPRE2_A::DIV2,
                apb3_div: device::rcc::d1cfgr::D1PPRE_A::DIV2,
                apb4_div: device::rcc::d3cfgr::D3PPRE_A::DIV2,

                // Flash runs at 200MHz: 2WS, 2 programming cycles. See
                // reference manual Table 13.
                flash_latency: 2,
                flash_write_delay: 2,
            };
        } else {
            compile_error!("target_board unknown or missing");
        }
    }

    //system_init(CLOCKS);

    // Use the crate peripheral take mechanism to get peripherals.
    let cp = cortex_m::Peripherals::take().unwrap();
    let p = device::Peripherals::take().unwrap();

    let p = system_init_custom(cp, p, config)
    // Enable the DACs and their associated timer.
    p.RCC.apb1lenr.modify(|_, w| {
        w.dac12en().set_bit()
        .tim6en().set_bit()
    });

    
    // Turn on profiling. We're sneaking around behind the GPIO driver's back
    // for this, but, it's a debug feature.
    //
    // We use the even pins (inside row) of CN8 for this. In order from board
    // north to south,
    // - 2 = PC8 = syscall number [0]
    // - 4 = PC9 = syscall number [1]
    // - 6 = PC10 = syscall number [2]
    // - 8 = PC11 = syscall number [3]
    // - 10 = PC12 = syscall active
    // - 12 = PD2 = pendsv
    // - 14 = PG2 = systick
    // - 16 = PG3 = general isr
    //
    // Using PC8:12 for this because it's available on even pins 2:12 of CN8,
    // labeled SDMMC.
    //
    // For task indices, we're using the SAI_A and top part of SAI_B on CN9.
    // From top of SAI_A down through SAI_B the bit order is:
    // - bit 0 (PE2)
    // - bit 2 (PE4)
    // - bit 3 (PE5)
    // - bit 4 (PE6)
    // - bit 1 (PE3)
    let rcc = unsafe { &*device::RCC::ptr() };
    #[rustfmt::skip]
    rcc.ahb4enr.modify(|_, w| {
        w.gpiocen().set_bit()
            .gpioden().set_bit()
            .gpioeen().set_bit()
            .gpiogen().set_bit()
    });
    cortex_m::asm::dmb();
    let gpioc = unsafe { &*device::GPIOC::ptr() };
    #[rustfmt::skip]
    gpioc.moder.modify(|_, w| {
        w.moder8().output()
            .moder9().output()
            .moder10().output()
            .moder11().output()
            .moder12().output()
    });
    let gpiod = unsafe { &*device::GPIOD::ptr() };
    #[rustfmt::skip]
    gpiod.moder.modify(|_, w| {
        w.moder2().output()
    });
    let gpioe = unsafe { &*device::GPIOE::ptr() };
    #[rustfmt::skip]
    gpioe.moder.modify(|_, w| {
        w.moder2().output()
            .moder3().output()
            .moder4().output()
            .moder5().output()
            .moder6().output()
    });
    let gpiog = unsafe { &*device::GPIOG::ptr() };
    #[rustfmt::skip]
    gpiog.moder.modify(|_, w| {
        w.moder2().output()
            .moder3().output()
    });

    kern::profiling::configure_events_table(&PROFILING);

    unsafe { kern::startup::start_kernel(CYCLES_PER_MS) }
}

fn syscall_enter(nr: u32) {
    let gpioc = unsafe { &*device::GPIOC::ptr() };
    gpioc.bsrr.write(|w| {
        if nr & 1 != 0 {
            w.bs8().set_bit();
        }
        if nr & 2 != 0 {
            w.bs9().set_bit();
        }
        if nr & 4 != 0 {
            w.bs10().set_bit();
        }
        if nr & 8 != 0 {
            w.bs11().set_bit();
        }
        w.bs12().set_bit();
        w
    });
}

fn syscall_exit() {
    let gpioc = unsafe { &*device::GPIOC::ptr() };
    #[rustfmt::skip]
    gpioc.bsrr.write(|w| {
        w.br8().set_bit()
            .br9().set_bit()
            .br10().set_bit()
            .br11().set_bit()
            .br12().set_bit()
    });
}

fn secondary_syscall_enter() {
    let gpiod = unsafe { &*device::GPIOD::ptr() };
    gpiod.bsrr.write(|w| w.bs2().set_bit());
}

fn secondary_syscall_exit() {
    let gpiod = unsafe { &*device::GPIOD::ptr() };
    gpiod.bsrr.write(|w| w.br2().set_bit());
}

fn isr_enter() {
    let gpiog = unsafe { &*device::GPIOG::ptr() };
    gpiog.bsrr.write(|w| w.bs3().set_bit());
}

fn isr_exit() {
    let gpiog = unsafe { &*device::GPIOG::ptr() };
    gpiog.bsrr.write(|w| w.br3().set_bit());
}

fn timer_isr_enter() {
    let gpiog = unsafe { &*device::GPIOG::ptr() };
    gpiog.bsrr.write(|w| w.bs2().set_bit());
}

fn timer_isr_exit() {
    let gpiog = unsafe { &*device::GPIOG::ptr() };
    gpiog.bsrr.write(|w| w.br2().set_bit());
}

fn context_switch(addr: usize) {
    let addr = addr >> 4;
    let gpioe = unsafe { &*device::GPIOE::ptr() };
    gpioe.bsrr.write(|w| {
        if addr & 1 != 0 {
            w.bs2().set_bit();
        } else {
            w.br2().set_bit();
        }
        if addr & 2 != 0 {
            w.bs3().set_bit();
        } else {
            w.br3().set_bit();
        }
        if addr & 4 != 0 {
            w.bs4().set_bit();
        } else {
            w.br4().set_bit();
        }
        if addr & 8 != 0 {
            w.bs5().set_bit();
        } else {
            w.br5().set_bit();
        }
        if addr & 16 != 0 {
            w.bs6().set_bit();
        } else {
            w.br6().set_bit();
        }
        w
    });
}

static PROFILING: kern::profiling::EventsTable = kern::profiling::EventsTable {
    syscall_enter,
    syscall_exit,
    secondary_syscall_enter,
    secondary_syscall_exit,
    isr_enter,
    isr_exit,
    timer_isr_enter,
    timer_isr_exit,
    context_switch,
};
