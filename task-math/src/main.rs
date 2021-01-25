#![no_std]
#![no_main]

use lib_casper::*;
use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    cortex_m_semihosting::hprintln!(
        "It was my understanding there would be no math"
    );

    lib_casper::casper_init();

    let mut c = 0x0 as u32;
    let mut d = 0x0 as u32;

    lib_casper::caspar_add64(0x1111_1111, 0x2222_2222, &mut c, &mut d);

    cortex_m_semihosting::hprintln!("math {:x} {:x}", c, d);

    loop {
        // NOTE: you need to put code here before running this! Otherwise LLVM
        // will turn this into a single undefined instruction.
    }
}
