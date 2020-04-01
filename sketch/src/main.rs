#![no_std]
#![no_main]

// extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_itm; // logs messages over ITM; requires ITM support

use cortex_m::asm;
use cortex_m_rt::entry;

#[entry]
fn main() -> ! {
    asm::nop(); // To not have main optimize to abort in release mode, remove when you add code

    loop {
        // your code goes here
    }
}
