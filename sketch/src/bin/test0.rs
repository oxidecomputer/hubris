#![no_std]
#![no_main]

extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
//extern crate panic_itm; // logs messages over ITM; requires ITM support

use sketch::*;

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {

    let dest = TaskName(42);
    let request = b"ohai there";
    let mut response = [0; 32];
    
    loop {
        let resp_len = send_untyped(dest, request, &mut response, &[])
            .expect("oh what now");
    }

}
