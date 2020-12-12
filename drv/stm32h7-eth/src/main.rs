//! STM32H7 Ethernet Server.

#![no_std]
#![no_main]
#![feature(min_const_generics)]

mod ring;

use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    panic!()
}
