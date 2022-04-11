// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the AT24CSW080 EEPROM
//!
//! Use the `eeprom-api` crate to interact with this driver.

#![no_std]
#![no_main]

use drv_i2c_api::ResponseCode;
use drv_i2c_devices::at24csw080::*;
use idol_runtime::RequestError;
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
task_slot!(I2C, i2c_driver);

struct EepromServer {
    dev: At24csw080,
}

/// The simplest possible EEPROM implementation.
///
/// This is _very inefficient_; writing byte-by-byte incurs a 5ms pause after
/// each byte, which could be reduced by writing 16-byte pages.
impl idl::InOrderEepromImpl for EepromServer {
    fn read_byte(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u16,
    ) -> Result<u8, RequestError<ResponseCode>> {
        self.dev.read::<u8>(addr).map_err(|e| e.into())
    }

    fn write_byte(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u16,
        value: u8,
    ) -> Result<(), RequestError<ResponseCode>> {
        self.dev.write_byte(addr, value).map_err(|e| e.into())
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let dev =
        At24csw080::new(i2c_config::devices::at24csw080_local(i2c_task)[0]);
    let mut srv = EepromServer { dev };
    let mut buffer = [0u8; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut srv);
    }
}

mod idl {
    use drv_i2c_api::ResponseCode;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
