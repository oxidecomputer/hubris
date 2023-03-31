// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the PSC sequencing process.

#![no_std]
#![no_main]

use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_psc_seq_api::PowerState;
use drv_stm32xx_sys_api as sys_api;
use task_jefe_api::Jefe;
use userlib::*;

task_slot!(SYS, sys);
task_slot!(I2C, i2c_driver);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

const STATUS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    // Turn off the chassis LED, in case this is a task restart (and not a
    // full chip restart, which would leave the GPIO unconfigured).
    sys.gpio_configure_output(
        STATUS_LED,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
    sys.gpio_reset(STATUS_LED);

    let jefe = Jefe::from(JEFE.get_task_id());

    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    jefe.set_state(PowerState::A2 as u32);
    sys.gpio_set(STATUS_LED);

    // We have nothing else to do, so sleep forever via waiting for a message
    // from the kernel that won't arrive.
    loop {
        _ = sys_recv_closed(&mut [], 0, TaskId::KERNEL);
    }
}
