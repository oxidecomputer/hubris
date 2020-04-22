#![no_std]
#![no_main]
#![feature(asm)]

extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics

#[link_section = ".text.start"]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    safe_main()
}

fn safe_main() -> ! {
    let peer = userlib::TaskId::for_index_and_gen(1, 0);
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    loop {
        // Signal that we're entering send:
        set_led();

        let (_code, _len) = userlib::sys_send(
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
