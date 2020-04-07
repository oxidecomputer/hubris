#![no_std]
#![no_main]

extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
//extern crate panic_itm; // logs messages over ITM; requires ITM support

use sketch::*;

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    let dest = TaskName(42);
    let op = 1;
    let request = b"ohai there";
    let mut response = [0; 32];

    let lent_for_read = b"I am a static array";
    let mut lent_for_write = [0; 1024];
    
    loop {
        let (code, _len) = sys_send(dest, op, request, &mut response, &[
            Lease::read(lent_for_read),
            Lease::write(&mut lent_for_write),
        ]);
        if code == 0 {
            // Ignore responses of any length.
        } else {
            // Panic on any peer error -- why not
            panic!()
        }
    }
}
