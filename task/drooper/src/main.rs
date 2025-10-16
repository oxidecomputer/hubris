// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! drooper: A task to simulate the IBC droop seen in mfg-quality#140
//! 
//!

#![no_std]
#![no_main]

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
#[allow(unused_imports)]
use userlib::*;

task_slot!(I2C, i2c_driver);


#[export_name = "main"]
fn main() -> ! {
    loop {
        // NOTE: you need to put code here before running this! Otherwise LLVM
        // will turn this into a single undefined instruction.

    }
}

struct ServerImpl {
}

impl idl::InOrderDrooperImpl for ServerImpl {
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
