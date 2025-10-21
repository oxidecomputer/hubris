// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! drooper: A task to simulate the IBC droop seen in mfg-quality#140
//! 
//! drooper.droop -a n=8
//! TODO: add interval to idl api
//!

#![no_std]
#![no_main]


use drv_i2c_devices::bmr491::*;

use core::convert::Infallible;
use idol_runtime::RequestError;
use userlib::{task_slot, RecvMessage, UnwrapLite};

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
#[allow(unused_imports)]
use userlib::*;

task_slot!(I2C, i2c_driver);


#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {};
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
}

impl idl::InOrderDrooperImpl for ServerImpl {
    fn droop(&mut self, msg: &RecvMessage, time_ms: u32) -> Result<(), RequestError<Infallible>> {
        let (device, rail) = i2c_config::pmbus::v12_sys_a2(I2C.get_task_id());
        let ibc = Bmr491::new(&device, rail);

        // Droop the voltage for the requested time period in ms.
        let _ = ibc.set_vout(9);
        userlib::hl::sleep_for(time_ms as u64);
        let _ = ibc.set_vout(12);

        Ok(())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // TODO: interval
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}


include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
