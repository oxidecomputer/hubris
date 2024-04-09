// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO sequencing process.

#![no_std]
#![no_main]

use drv_sidecar_front_io::sequencer::FrontIOBoard;

use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[export_name = "main"]
fn main() -> ! {
    let mut front_io_board =
        FrontIOBoard::new(FRONT_IO.get_task_id(), AUXFLASH.get_task_id());

    front_io_board.init().unwrap_lite();

    loop {}
}
