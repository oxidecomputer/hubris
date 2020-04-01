#![no_std]
#![no_main]

// extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_itm; // logs messages over ITM; requires ITM support

use cortex_m::asm;

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    asm::nop(); // To not have main optimize to abort in release mode, remove when you add code

    loop {
        // your code goes here
    }
}
