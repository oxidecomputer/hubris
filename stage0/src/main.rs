#![feature(cmse_nonsecure_entry)]
#![feature(asm)]
#![feature(naked_functions)]
#![feature(array_methods)]
#![no_main]
#![no_std]

extern crate panic_halt;
use crate::attest::{attest, validate_image};
use cortex_m::peripheral::Peripherals;
use cortex_m_rt::entry;

mod attest;
mod hypo;
mod image_header;
mod puf;

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

#[inline(never)]
fn write_sau() {
    extern "C" {
        static address_of_start_flash_hypo: u32;
        static address_of_end_flash_hypo: u32;
    }

    let sau_ctrl: *mut u32 = 0xe000edd0 as *mut u32;
    let sau_rbar: *mut u32 = 0xe000eddc as *mut u32;
    let sau_rlar: *mut u32 = 0xe000ede0 as *mut u32;
    let sau_rnr: *mut u32 = 0xe000edd8 as *mut u32;

    // By default anything not in the SAU is secure
    unsafe {
        let hypo_start = address_of_start_flash_hypo as *const u32 as u32;
        let hypo_end = address_of_end_flash_hypo as *const u32 as u32;

        let img_flash = address_of_imagea_flash as *const u32 as u32;
        let img_ram = address_of_imagea_ram as *const u32 as u32;

        // this is the dedicated entry function
        core::ptr::write_volatile(sau_rnr, 0);
        core::ptr::write_volatile(sau_rbar, hypo_start);
        // enable and set NS callable
        core::ptr::write_volatile(sau_rlar, hypo_end | 0x3);

        // The rest of the flash is non-secure
        core::ptr::write_volatile(sau_rnr, 1);
        core::ptr::write_volatile(sau_rbar, img_flash);
        core::ptr::write_volatile(sau_rlar, 0x0fff_ffe0 | 1);

        // non secure RAM
        core::ptr::write_volatile(sau_rnr, 2);
        core::ptr::write_volatile(sau_rbar, img_ram);
        core::ptr::write_volatile(sau_rlar, 0x2fff_ffe0 | 1);

        // non-secure peripherals
        core::ptr::write_volatile(sau_rnr, 3);
        core::ptr::write_volatile(sau_rbar, 0x4000_0000);
        core::ptr::write_volatile(sau_rlar, 0x4fff_ffe0 | 1);

        // Actually enable the SAU
        core::ptr::write_volatile(sau_ctrl, 1);
    }
}

extern "C" {
    static address_of_imagea_flash: u32;
    static address_of_imagea_ram: u32;
}

#[entry]
fn main() -> ! {
    let imagea = image_header::get_image_a().unwrap();

    let mut image_size: u32 = 0;
    let mut entry_pt: u32 = 0;
    let mut stack: u32 = 0;
    let mut image_hash = [0u8; 32];

    if let Err(_) = validate_image(
        &imagea,
        &mut image_size,
        &mut image_hash,
        &mut entry_pt,
        &mut stack,
    ) {
        panic!("Image signature check failed");
    }

    if let Err(_) = attest(image_size, &image_hash, entry_pt) {
        panic!("Attestation failed");
    }

    let mut peripherals = Peripherals::take().unwrap();

    unsafe {
        write_sau();

        // Allow nonsecure access to cp10/11 (i.e. the floating point unit)
        core::ptr::write_volatile(0xE000ED8C as *mut u32, 0xc00);

        peripherals
            .SCB
            .enable(cortex_m::peripheral::scb::Exception::SecureFault);
        peripherals
            .SCB
            .enable(cortex_m::peripheral::scb::Exception::UsageFault);
        peripherals
            .SCB
            .enable(cortex_m::peripheral::scb::Exception::BusFault);

        // Set BFHFNMINS (Bus Fault, Hard Fault, NMI non-secure)
        core::ptr::write_volatile(0xe000ed0c as *mut u32, 0x05fa2000);

        let vector = entry_pt & !1u32;

        asm!("msr MSP_NS, {}", in(reg) stack);

        // Write the NS VTOR
        core::ptr::write_volatile(
            0xE002ED08 as *mut u32,
            imagea.get_img_start(),
        );

        // and branch
        asm!("bxns {}", in(reg) vector, options(noreturn), );
    }
}
