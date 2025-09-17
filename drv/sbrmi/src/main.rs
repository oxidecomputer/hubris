// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Task for communicating to the host CPU via the SB-RMI interface.

#![no_std]
#![no_main]

use drv_i2c_devices::sbrmi::{CpuidResult, Sbrmi};
use drv_sbrmi_api::SbrmiError;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use userlib::*;
use zerocopy::{FromBytes, Immutable, KnownLayout};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl {
    sbrmi: Sbrmi,
}

task_slot!(I2C, i2c_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    CpuidError(drv_i2c_devices::sbrmi::Error),
    CpuidResult(CpuidResult),
    Rdmsr(u32),
    RdmsrError(drv_i2c_devices::sbrmi::Error),
    RdmsrOk,
}

ringbuf!(Trace, 16, Trace::None);

impl ServerImpl {
    fn rdmsr<T: FromBytes + Immutable + KnownLayout>(
        &self,
        thread: u8,
        msr: u32,
    ) -> Result<T, RequestError<SbrmiError>> {
        ringbuf_entry!(Trace::Rdmsr(msr));

        match self.sbrmi.rdmsr::<T>(thread, msr) {
            Err(code) => {
                ringbuf_entry!(Trace::RdmsrError(code));
                Err(SbrmiError::from(code).into())
            }
            Ok(rval) => {
                ringbuf_entry!(Trace::RdmsrOk);
                Ok(rval)
            }
        }
    }
}

impl idl::InOrderSbrmiImpl for ServerImpl {
    fn nthreads(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<SbrmiError>> {
        self.sbrmi
            .nthreads()
            .map_err(|code| RequestError::from(SbrmiError::from(code)))
    }

    fn enabled(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 16], RequestError<SbrmiError>> {
        self.sbrmi
            .enabled()
            .map_err(|code| RequestError::from(SbrmiError::from(code)))
    }

    fn alert(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 16], RequestError<SbrmiError>> {
        self.sbrmi
            .alert()
            .map_err(|code| RequestError::from(SbrmiError::from(code)))
    }

    fn cpuid(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        eax: u32,
        ecx: u32,
    ) -> Result<[u32; 4], RequestError<SbrmiError>> {
        match self.sbrmi.cpuid(thread, eax, ecx) {
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

    fn rdmsr8(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        msr: u32,
    ) -> Result<u8, RequestError<SbrmiError>> {
        self.rdmsr::<u8>(thread, msr)
    }

    fn rdmsr16(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        msr: u32,
    ) -> Result<u16, RequestError<SbrmiError>> {
        self.rdmsr::<u16>(thread, msr)
    }

    fn rdmsr32(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        msr: u32,
    ) -> Result<u32, RequestError<SbrmiError>> {
        self.rdmsr::<u32>(thread, msr)
    }

    fn rdmsr64(
        &mut self,
        _: &RecvMessage,
        thread: u8,
        msr: u32,
    ) -> Result<u64, RequestError<SbrmiError>> {
        self.rdmsr::<u64>(thread, msr)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let devs = i2c_config::devices::sbrmi(I2C.get_task_id());
    let mut server = ServerImpl {
        sbrmi: Sbrmi::new(&devs[0]),
    };

    let mut incoming = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use super::SbrmiError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
