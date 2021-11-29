// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use cortex_m_semihosting::hprintln;
use userlib::*;

task_slot!(I2C, i2c_driver);

#[export_name = "main"]
fn main() -> ! {
    let addr: &[u8] = &[0x0];
    let i2c = I2C.get_task_id();
    hprintln!("Starting to spam!").ok();
    loop {
        let mut recv: [u8; 4] = [0; 4];
        let a: &mut [u8] = &mut recv;
        // This is address of the WM8904 on Flexcomm 4
        // register 0 = id register that should always return 8904 on read
        let (code, _) =
            sys_send(i2c, 1, &[0x1a], &mut [], &[Lease::from(addr)]);
        if code != 0 {
            hprintln!("Got error code{}", code).ok();
        } else {
            hprintln!("Success").ok();
        }
        let (code, _) = sys_send(i2c, 2, &[0x1a], &mut [], &[Lease::from(a)]);
        if code != 0 {
            hprintln!("Got error code{}", code).ok();
        } else {
            hprintln!("Got buffer {:x?}", recv).ok();
        }
    }
}
