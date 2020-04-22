#![no_std]
#![no_main]
#![feature(asm)]

extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    safe_main()
}

fn safe_main() -> ! {
    let peer = userlib::TaskId::for_index_and_gen(1, 0);
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    loop {
        let (_code, _len) = userlib::sys_send(
            peer,
            PING_OP,
            b"hello",
            &mut response,
            &[],
        );
    }
}
