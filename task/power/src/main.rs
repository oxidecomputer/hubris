// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_i2c_devices::isl68224::*;
use ringbuf::*;
use userlib::units::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[derive(Copy, Clone, PartialEq)]
#[allow(dead_code)]
enum Device {
    Adm1272,
    Tps546b24a,
    Isl68224,
}

#[derive(Copy, Clone, PartialEq)]
#[allow(dead_code)]
enum Command {
    VIn(Volts),
    VOut(Volts),
    IOut(Amperes),
    PeakIOut(Amperes),
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Datum(Device, Command),
    None,
}

ringbuf!(Trace, 16, Trace::None);

fn trace(dev: Device, cmd: Command) {
    ringbuf_entry!(Trace::Datum(dev, cmd));
}

#[export_name = "main"]
fn main() -> ! {
    let task = I2C.get_task_id();

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            let (device, rail) = i2c_config::pmbus::isl_evl_vout0(task);
            let mut isl0 = Isl68224::new(&device, rail);

            let (device, rail) = i2c_config::pmbus::isl_evl_vout1(task);
            let mut isl1 = Isl68224::new(&device, rail);
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = &i2c_config::devices::mock(task);
                    let mut isl0 = Isl68224::new(&device, 0);
                    let mut isl1 = Isl68224::new(&device, 0);
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        isl0.turn_off().unwrap();
        isl1.turn_on().unwrap();
        hl::sleep_for(1000);

        isl0.turn_on().unwrap();
        isl1.turn_off().unwrap();
        hl::sleep_for(1000);

        let vout = isl0.read_vout().unwrap();
        trace(Device::Isl68224, Command::VOut(vout));

        let vout = isl1.read_vout().unwrap();
        trace(Device::Isl68224, Command::VOut(vout));
    }
}
