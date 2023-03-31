// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![feature(naked_functions)]
#![feature(array_methods)]
#![no_main]
#![no_std]

use core::arch;

extern crate lpc55_pac;
extern crate panic_halt;
use armv8_m_mpu::{disable_mpu, enable_mpu};
use cortex_m::peripheral::Peripherals as CorePeripherals;
use cortex_m::peripheral::MPU;
use cortex_m_rt::entry;
use lpc55_pac::Peripherals;
use unwrap_lite::UnwrapLite;

#[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
mod dice;
#[cfg(feature = "dice-mfg")]
mod dice_mfg_usart;

// FIXME Need to fixup the secure interface calls
//mod hypo;
mod handoff;
mod image_header;
use handoff::Handoff;

use crate::image_header::Image;

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

const ROM_VER: u32 = 1;

#[cfg(feature = "tz_support")]
unsafe fn config_platform(image: &Image) {
    let sau_ctrl: *mut u32 = 0xe000edd0 as *mut u32;
    let sau_rbar: *mut u32 = 0xe000eddc as *mut u32;
    let sau_rlar: *mut u32 = 0xe000ede0 as *mut u32;
    let sau_rnr: *mut u32 = 0xe000edd8 as *mut u32;

    for i in 0..8 {
        if let Some(r) = image.get_sau_entry(i) {
            core::ptr::write_volatile(sau_rnr, i as u32);
            core::ptr::write_volatile(sau_rbar, r.rbar);
            core::ptr::write_volatile(sau_rlar, r.rlar);
        }
    }

    core::ptr::write_volatile(sau_ctrl, 1);

    let mut peripherals = CorePeripherals::steal();

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
    core::ptr::write_volatile(0xE002ED08 as *mut u32, image.get_vectors());

    // Route all interrupts to the NS world
    // TODO use only the interrupts we've enabled
    core::ptr::write_volatile(0xe000e380 as *mut u32, 0xffffffff);
    core::ptr::write_volatile(0xe000e384 as *mut u32, 0xffffffff);
}

#[cfg(feature = "tz_support")]
unsafe fn branch_to_image(image: Image) -> ! {
    // For secure we do not set the thumb bit!
    let entry_pt = image.get_pc() & !1u32;
    let stack = image.get_sp();

    // and branch
    arch::asm!("
            msr MSP_NS, {stack}
            bxns {entry}",
        stack = in(reg) stack,
        entry = in(reg) entry_pt,
        options(noreturn),
    );
}

#[cfg(not(feature = "tz_support"))]
unsafe fn config_platform(image: &Image) {
    let mut peripherals = CorePeripherals::steal();

    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::UsageFault);
    peripherals
        .SCB
        .enable(cortex_m::peripheral::scb::Exception::BusFault);

    // Write the VTOR
    core::ptr::write_volatile(0xE000ED08 as *mut u32, image.get_vectors());
}

#[cfg(not(feature = "tz_support"))]
unsafe fn branch_to_image(image: Image) -> ! {
    let entry_pt = image.get_pc();
    let stack = image.get_sp();

    // and branch
    arch::asm!("
            msr MSP, {stack}
            bx {entry}",
        stack = in(reg) stack,
        entry = in(reg) entry_pt,
        options(noreturn),
    );
}

// Setup the MPU so that we can treat the USB RAM as normal RAM, and not as a
// peripheral. Specifically we want to clear the `DEVICE` attributes, so that
// we can allow unaligned access.
fn apply_memory_protection(mpu: MPU) {
    unsafe {
        disable_mpu(&mpu);
    }

    const USB_RAM_BASE: u32 = 0x4010_0000;
    const USB_RAM_SIZE: u32 = 0x4000;
    const USB_RAM_REGION_NUMBER: u32 = 0;

    // Subtract 32 because the `LIMIT` field in the `rlar` register range is inclusive
    // and then enable the region (bit 0).
    let rlar = (USB_RAM_BASE + USB_RAM_SIZE - 32) | (1 << 0);

    let ap = 0b01; // read-write by any privilege level
    let sh = 0b00; // non-shareable - we only use one core with no DMA here
    let xn = 1; // Don't execute out of this region
    let rbar = USB_RAM_BASE | (sh as u32) << 3 | ap << 1 | xn;

    // region 0: write-back transient, not shared, with read/write supported
    let mair0 = 0b0111_0100;

    unsafe {
        mpu.rnr.write(USB_RAM_REGION_NUMBER);
        // We only have one region (0), so no need for a RMW cycle
        mpu.mair[0].write(mair0);
        mpu.rbar.write(rbar);
        mpu.rlar.write(rlar);
    }

    unsafe {
        enable_mpu(&mpu, true);
    }
}

#[entry]
fn main() -> ! {
    // This is the SYSCON_DIEID register on LPC55 which contains the ROM
    // version. Make sure our configuration matches!
    let val = unsafe { core::ptr::read_volatile(0x50000ffc as *const u32) };

    if val & 1 != ROM_VER {
        panic!()
    }

    let mpu = CorePeripherals::take().unwrap_lite().MPU;

    // Turn on the memory used by the handoff subsystem to dump
    // `RotUpdateDetails` and DICE information required by hubris.
    //
    // This allows hubris tasks to always get valid memory, even if it is all
    // 0's.
    let peripherals = Peripherals::take().unwrap_lite();
    let handoff = Handoff::turn_on(&peripherals.SYSCON);

    apply_memory_protection(mpu);

    let (image, _) = image_header::select_image_to_boot();
    unsafe {
        config_platform(&image);
    }
    image_header::dump_image_details_to_ram(&handoff);

    #[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
    dice::run(&image, &handoff, &peripherals);

    unsafe {
        branch_to_image(image);
    }
}

include!(concat!(env!("OUT_DIR"), "/consts.rs"));
