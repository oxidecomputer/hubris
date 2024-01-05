// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Medusa sequencing process.

#![no_std]
#![no_main]

use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

// const TIMER_INTERVAL: u64 = 1000;

// struct ServerImpl {
// }

// impl ServerImpl {
// }

// impl NotificationHandler for ServerImpl {
//     fn current_notification_mask(&self) -> u32 {
//         notifications::TIMER_MASK
//     }

//     fn handle_notification(&mut self, _bits: u32) {
//         let next_deadline = sys_get_timer().now + TIMER_INTERVAL;

//         sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
//     }
// }

#[export_name = "main"]
fn main() -> ! {

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    // let deadline = sys_get_timer().now;
    // sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    loop {
        
    }
}
