// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! drooper: A task to simulate the IBC droop seen in mfg-quality#140
//!
//! Running this task will cause all U.2s to undergo a PCIe reset event.
//! (Use with caution!)
//!
//! For example, to drop the voltage for 30 ms:
//! $ humility hiffy -c drooper.droop -a time_ms=30
//!

#![no_std]
#![no_main]

use drv_i2c_devices::bmr491::*;

use core::convert::Infallible;
use idol_runtime::RequestError;
use userlib::{task_slot, RecvMessage, UnwrapLite};

task_slot!(I2C, i2c_driver);

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {};
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {}

impl idl::InOrderDrooperImpl for ServerImpl {
    fn droop(
        &mut self,
        _msg: &RecvMessage,
        time_ms: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let (device, rail) = i2c_config::pmbus::v12_sys_a2(I2C.get_task_id());
        let ibc = Bmr491::new(&device, rail);

        // Droop the voltage for the requested time period in ms.
        // We pick 9V because it's approximately what we see in the field for mfg-quality#140.
        let _ = ibc.set_vout(9);
        userlib::hl::sleep_for(time_ms as u64);

        // Restore to 12V.
        let _ = ibc.set_vout(12);

        Ok(())
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
