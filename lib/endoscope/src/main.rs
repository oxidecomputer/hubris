// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// use panic_halt as _;
use core::arch::{self};
use core::panic::PanicInfo;
use cortex_m_rt::entry;
use drv_stm32h7_startup::system_init;
use drv_stm32h7_startup::ClockConfig;
use endoscope_abi::DIGEST_SIZE;
use stm32h7::stm32h753 as device;

use sha3::{Digest, Sha3_256};

// The clock settings for Gimlet, PSC, Sidecar, and Grapefruit, as well as the
// STM32H753 Nucleo board, are all the same. Refactor when new incompatible boards are added.
const CLOCK_CONFIG: ClockConfig = ClockConfig {
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
};

mod shared;
use shared::{State, SHARED};

extern "C" {
    static FLASH_BASE: [u8; 0];
    static FLASH_SIZE: [u32; 0];
}

#[entry]
fn main() -> ! {
    // Note: The RoT does not examine results until the SP is halted.
    SHARED.set_state(State::Running);
    let _p = system_init(CLOCK_CONFIG);
    let mut hash = Sha3_256::new();

    // Safety: The bounds of the device's flash area are link-time constants.
    let image = unsafe {
        core::slice::from_raw_parts(
            FLASH_BASE.as_ptr() as u32 as *const u8,
            FLASH_SIZE.as_ptr() as u32 as usize,
        )
    };

    hash.update(image[..].as_ref());
    let digest: [u8; DIGEST_SIZE] = hash.finalize().into();
    SHARED.set_digest(&digest);
    SHARED.set_state(State::Done);
    panic!(); // Need to trap so that RoT can intercept.
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    breakpoint();
}

#[allow(non_snake_case)]
#[no_mangle]
/// # Safety
/// Required by the architecture and linker.
pub unsafe extern "C" fn DefaultHandler() {
    breakpoint();
}

#[no_mangle]
extern "C" fn breakpoint() -> ! {
    loop {
        unsafe {
            arch::asm!(
                "
                .globl break
                bkpt
            "
            );
            // noreturn
        }
    }
}
