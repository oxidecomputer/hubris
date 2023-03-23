// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Device validation

#![no_std]
#![no_main]

use idol_runtime::RequestError;
use ringbuf::*;
use task_validate_api::{MuxSegment, ValidateError, ValidateOk};
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Validate(usize),
    ValidateFailure(drv_i2c_api::ResponseCode),
    None,
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

    //
    // A quick-and-dirty entry point to indicate the last mux and segment to
    // aid in debugging locked I2C bus conditions.
    //
    fn selected_mux_segment(
        &mut self,
        _: &RecvMessage,
        index: u32,
    ) -> Result<Option<MuxSegment>, RequestError<ValidateError>> {
        use i2c_config::devices::{lookup_controller, lookup_port};

        let index = index as usize;
        let c = lookup_controller(index).ok_or(ValidateError::InvalidDevice)?;
        let p = lookup_port(index).ok_or(ValidateError::InvalidDevice)?;

        let task = I2C.get_task_id();
        let device = drv_i2c_api::I2cDevice::new(task, c, p, None, 0);

        match device.selected_mux_segment() {
            Ok(None) => Ok(None),
            Ok(Some((mux, segment))) => Ok(Some(MuxSegment { mux, segment })),
            Err(err) => {
                let err: ValidateError = err.into();
                Err(err.into())
            }
        }
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
    use super::{MuxSegment, ValidateError, ValidateOk};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}