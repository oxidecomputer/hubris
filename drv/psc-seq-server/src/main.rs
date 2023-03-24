// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the PSC sequencing process.

#![no_std]
#![no_main]

use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_psc_seq_api::PowerState;
use task_jefe_api::Jefe;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

#[export_name = "main"]
fn main() -> ! {
    let jefe = Jefe::from(JEFE.get_task_id());

    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    jefe.set_state(PowerState::A2 as u32);

    // We have nothing else to do, so sleep forever via waiting for a message
    // from the kernel that won't arrive.
    loop {
        _ = sys_recv_closed(&mut [], 0, TaskId::KERNEL);
    }
}
