// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![feature(cmse_nonsecure_entry)]
#![feature(asm)]
#![feature(naked_functions)]
#![feature(array_methods)]
#![no_main]
#![no_std]

extern crate panic_halt;
use cortex_m::peripheral::Peripherals;
use cortex_m_rt::entry;

mod hypo;
mod image_header;

use crate::image_header::ImageHeader;

/// Initial entry point for handling a memory management fault.
#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn MemoryManagement() {
    loop {}
}

/// Initial entry point for handling a bus fault.
#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn BusFault() {
    loop {}
}

/// Initial entry point for handling a usage fault.
#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn UsageFault() {
    loop {}
}

#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn SecureFault() {
    loop {}
}

// These correspond to REV_ID in the SYSCON_DIEID field
#[cfg(feature = "0A-hardware")]
const ROM_VER: u32 = 0;

#[cfg(not(feature = "0A-hardware"))]
const ROM_VER: u32 = 1;

#[cfg(feature = "tz_support")]
unsafe fn branch_to_image(image: &'static ImageHeader) -> ! {
    let sau_ctrl: *mut u32 = 0xe000edd0 as *mut u32;
    let sau_rbar: *mut u32 = 0xe000eddc as *mut u32;
    let sau_rlar: *mut u32 = 0xe000ede0 as *mut u32;
    let sau_rnr: *mut u32 = 0xe000edd8 as *mut u32;

    // TODO our NSC region

    core::ptr::write_volatile(sau_rnr, 0);
    core::ptr::write_volatile(sau_rbar, 0x8000);
    core::ptr::write_volatile(sau_rlar, 0x0fff_ffe0 | 1);

    core::ptr::write_volatile(sau_rnr, 1);
    core::ptr::write_volatile(sau_rbar, 0x20004000);
    core::ptr::write_volatile(sau_rlar, 0x2fff_ffe0 | 1);

    core::ptr::write_volatile(sau_rnr, 2);
    core::ptr::write_volatile(sau_rbar, 0x4000_0000);
    core::ptr::write_volatile(sau_rlar, 0x4fff_ffe0 | 1);

    core::ptr::write_volatile(sau_ctrl, 1);

    let mut peripherals = match Peripherals::take() {
        Some(p) => p,
        None => loop {},
    };

    // let co processor be non-secure
    core::ptr::write_volatile(0xE000ED8C as *mut u32, 0xc00);

    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::UsageFault);
    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::BusFault);

    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::SecureFault);

    // Make our exceptions NS
    core::ptr::write_volatile(0xe000ed0c as *mut u32, 0x05fa2000);

    // Write the NS_VTOR
    core::ptr::write_volatile(0xE002ED08 as *mut u32, image.get_img_start());

    // For secure we do not set the thumb bit!
    let entry_pt = image.get_pc() & !1u32;
    let stack = image.get_sp();

    // and branch
    asm!("
            msr MSP_NS, {stack}
            bxns {entry}",
        stack = in(reg) stack,
        entry = in(reg) entry_pt,
        options(noreturn),
    );
}

#[cfg(not(feature = "tz_support"))]
unsafe fn branch_to_image(image: &'static ImageHeader) -> ! {
    let mut peripherals = match Peripherals::take() {
        Some(p) => p,
        None => loop {},
    };

    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::UsageFault);
    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::BusFault);

    // Write the VTOR
    core::ptr::write_volatile(0xE000ED08 as *mut u32, image.get_img_start());

    let entry_pt = image.get_pc();
    let stack = image.get_sp();

    // and branch
    asm!("
            msr MSP, {stack}
            bx {entry}",
        stack = in(reg) stack,
        entry = in(reg) entry_pt,
        options(noreturn),
    );
}

#[entry]
fn main() -> ! {
    // This is the SYSCON_DIEID register on LPC55 which contains the ROM
    // version. Make sure our configuration matches!
    let val = unsafe { core::ptr::read_volatile(0x50000ffc as *const u32) };

    if val & 1 != ROM_VER {
        loop {}
    }

    let imagea = match image_header::get_image_a() {
        Some(a) => a,
        None => loop {},
    };

    unsafe {
        branch_to_image(imagea);
    }
}
