// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Device validation

#![no_std]
#![no_main]

use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use task_validate_api::{ValidateError, ValidateOk};
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Validate(usize),
    ValidateFailure(drv_i2c_api::ResponseCode),
}

ringbuf!(Trace, 64, Trace::None);

task_slot!(I2C, i2c_driver);

impl idl::InOrderValidateImpl for ServerImpl {
    fn validate_i2c(
        &mut self,
        _: &RecvMessage,
        index: u32,
    ) -> Result<ValidateOk, RequestError<ValidateError>> {
        use i2c_config::validation::I2cValidation;

        let index = index as usize;
        ringbuf_entry!(Trace::Validate(index));

        match i2c_config::validation::validate(I2C.get_task_id(), index) {
            Err(err) => {
                ringbuf_entry!(Trace::ValidateFailure(err));
                let err: ValidateError = err.into();
                Err(err.into())
            }
            Ok(ok) => match ok {
                I2cValidation::RawReadOk => Ok(ValidateOk::Present),
                I2cValidation::Good => Ok(ValidateOk::Validated),
                I2cValidation::Bad => Err(ValidateError::BadValidation.into()),
            },
        }
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl;
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{ValidateError, ValidateOk};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
