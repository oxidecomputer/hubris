#![no_std]
#![no_main]

// you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_halt;

#[export_name = "main"]
fn main() -> ! {
    loop {
        // NOTE: you need to put code here before running this! Otherwise LLVM
        // will turn this into a single undefined instruction.
    }
}
