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

#[entry]
fn main() -> ! {
    // This is the SYSCON_DIEID register on LPC55 which contains the ROM
    // version. Make sure our configuration matches!
    let val = unsafe { core::ptr::read_volatile(0x50000ffc as *const u32) };

    if val & 1 != ROM_VER {
        loop {}
    }

    let imagea = image_header::get_image_a().unwrap();

    let entry_pt = imagea.get_pc();
    let stack = imagea.get_sp();

    let mut peripherals = Peripherals::take().unwrap();

    unsafe {
        peripherals
            .SCB
            .enable(cortex_m::peripheral::scb::Exception::UsageFault);
        peripherals
            .SCB
            .enable(cortex_m::peripheral::scb::Exception::BusFault);

        // Write the VTOR
        core::ptr::write_volatile(
            0xE000ED08 as *mut u32,
            imagea.get_img_start(),
        );

        // and branch
        asm!("
            msr MSP, {stack}
            bx {entry}",
            stack = in(reg) stack,
            entry = in(reg) entry_pt,
            options(noreturn),
        );
    }
}
