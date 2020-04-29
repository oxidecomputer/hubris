#![no_std]
#![no_main]
#![feature(asm)]

// you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_halt;

use userlib::*;

#[cfg(feature = "standalone")]
const PEER: Task = SELF;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[export_name = "main"]
fn main() -> ! {
    let peer = TaskId::for_index_and_gen(PEER as usize, 0);
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    loop {
        // Signal that we're entering send:
        set_led();

        let (_code, _len) = sys_send(
            peer,
            PING_OP,
            b"hello",
            &mut response,
            &[],
        );
    }
}

fn set_led() {
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.bsrr.write(|w| w.bs12().set_bit());
}
