// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_i2c_devices::sbrmi::{CpuidResult, Sbrmi};
use drv_sbrmi_api::SbrmiError;
use idol_runtime::RequestError;
use ringbuf::*;
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

task_slot!(I2C, i2c_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    CpuidError(drv_i2c_devices::sbrmi::Error),
    CpuidResult(CpuidResult),
}

ringbuf!(Trace, 16, Trace::None);

impl idl::InOrderSbrmiImpl for ServerImpl {
    fn cpuid(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        eax: u32,
        ecx: u32,
    ) -> Result<[u32; 4], RequestError<SbrmiError>> {
        let devs = i2c_config::devices::sbrmi(I2C.get_task_id());
        let dev = Sbrmi::new(&devs[0]);

        match dev.cpuid(thread, eax, ecx) {
            Err(code) => {
                ringbuf_entry!(Trace::CpuidError(code));
                let err = SbrmiError::from(code);
                Err(err.into())
            }
            Ok(rval) => {
                ringbuf_entry!(Trace::CpuidResult(rval));
                Ok([rval.eax, rval.ebx, rval.ecx, rval.edx])
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl;
    let mut incoming = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use super::SbrmiError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
