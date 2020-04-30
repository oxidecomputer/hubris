#![no_std]
#![no_main]
#![feature(asm)]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
extern crate userlib;

#[export_name = "main"]
fn main() -> ! {
    loop {
        // Safety: asm in general is unsafe, but this instruction is fine.
        unsafe {
            // Wait For Interrupt to pause the processor until an ISR arrives,
            // which could wake some higher-priority task.
            asm!("wfi"::::"volatile");
        }
    }
}
