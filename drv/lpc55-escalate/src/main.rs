#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
//extern crate userlib;
use userlib::*;
use zerocopy::AsBytes;

fn read_cpuid() {
    unsafe {
        // This is a privileged operation!
        let cpuid = core::ptr::read_volatile(0xE000ED00 as *mut u32);
        cortex_m_semihosting::hprintln!("good afternoon from ring0 {:x}", cpuid);
    }

    loop {}
}

fn do_patch() {
    // Address we're going to change
    //
    // This holds a function pointer for handling svc calls in the ROM
    let change_addr = 0x130002d4;

    // Register with some kind of settings (might be on/off?)
    let rom_patch_setting = 0x5003_e0f4;

    // Register that holds our patching address
    let rom_patch_target_addr = 0x5003_e100;

    // Register that holds the patching instruction
    let rom_patch_target_insn = 0x5003_e0f0;

    unsafe {
        // unlock the rom patching (guess based on disassembly)
        core::ptr::write_volatile(rom_patch_setting as *mut u32, 0x20000000);

        // Write the address we're changing (SVC function pointer)
        core::ptr::write_volatile(
            rom_patch_target_addr as *mut u32,
            change_addr,
        );

        // Make that SVC handler do something we like better
        core::ptr::write_volatile(
            rom_patch_target_insn as *mut u32,
            read_cpuid as u32,
        );

        // Re-enable the patching (guess is that each bit is what is actually
        // active
        core::ptr::write_volatile(rom_patch_setting as *mut u32, 0x7);

        cortex_m_semihosting::hprintln!("hello");

        // Use the ROM vector table instead of ours
        core::ptr::write_volatile(0x50000000 as *mut u32, 0x00000000);

        // Now make a system call
        sys_panic(b"patch didn't work :( :( :(");
    }
}

#[export_name = "main"]
fn main() -> ! {
    do_patch();

    loop {}
}
