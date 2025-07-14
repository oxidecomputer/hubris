// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Task for communicating to the host CPU via the SB-RMI interface.

#![no_std]
#![no_main]

use drv_i2c_devices::sbrmi::{self, BlockProto, ByteProto, CpuidResult};
use drv_sbrmi_api::SbrmiError;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use userlib::*;
use zerocopy::{FromBytes, Immutable, KnownLayout};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl {
    sbrmi: Option<sbrmi::SbRmi<ByteProto>>,
}

task_slot!(I2C, i2c_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    InitializationError(drv_i2c_devices::sbrmi::Error),
    CpuidError(drv_i2c_devices::sbrmi::Error),
    CpuidResult(CpuidResult),
    Rdmsr(u32),
    RdmsrError(drv_i2c_devices::sbrmi::Error),
    RdmsrOk,
}

ringbuf!(Trace, 16, Trace::None);

impl ServerImpl {
    fn with_sbrmi_block_proto<T, F>(
        &mut self,
        thunk: F,
    ) -> Result<T, sbrmi::Error>
    where
        F: FnOnce(&sbrmi::SbRmi<BlockProto>) -> Result<T, sbrmi::Error>,
    {
        let sbrmi = self.sbrmi.take().ok_or(sbrmi::Error::Unavailable)?;
        let block = sbrmi.into_block_proto()?;
        let res = thunk(&block);
        self.sbrmi = Some(block.into_byte_proto()?);
        res.into()
    }

    fn with_sbrmi_byte_proto<T, F>(&mut self, thunk: F) -> Result<T, SbrmiError>
    where
        F: FnOnce(&sbrmi::SbRmi<ByteProto>) -> Result<T, sbrmi::Error>,
    {
        let sbrmi = self.sbrmi.take().ok_or(sbrmi::Error::Unavailable)?;
        let res = thunk(&sbrmi).map_err(SbrmiError::from);
        self.sbrmi = Some(sbrmi);
        res
    }

    fn rdmsr<T: FromBytes + Immutable + KnownLayout>(
        &mut self,
        thread: u32,
        msr: u32,
    ) -> Result<T, RequestError<SbrmiError>> {
        ringbuf_entry!(Trace::Rdmsr(msr));
        match self.with_sbrmi_block_proto(|sbrmi| sbrmi.rdmsr(thread, msr)) {
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

impl idl::InOrderSbRmiImpl for ServerImpl {
    fn nthreads(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<SbrmiError>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi::SbRmi::<ByteProto>::nthreads)?)
    }

    fn enabled(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 32], RequestError<SbrmiError>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi::SbRmi::<ByteProto>::enabled)?)
    }

    fn alert(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 32], RequestError<SbrmiError>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi::SbRmi::<ByteProto>::alert)?)
    }

    fn mailbox(
        &mut self,
        _: &RecvMessage,
        cmd: drv_i2c_devices::sbrmi::MailboxCmd,
    ) -> Result<Option<u32>, RequestError<SbrmiError>> {
        Ok(self.with_sbrmi_byte_proto(|sbrmi| sbrmi.mailbox(cmd))?)
    }

    fn cpuid(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        eax: u32,
        ecx: u32,
    ) -> Result<[u32; 4], RequestError<SbrmiError>> {
        match self.with_sbrmi_block_proto(|sbrmi| sbrmi.cpuid(thread, eax, ecx))
        {
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
        thread: u32,
        msr: u32,
    ) -> Result<u8, RequestError<SbrmiError>> {
        self.rdmsr::<u8>(thread, msr)
    }

    fn rdmsr16(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        msr: u32,
    ) -> Result<u16, RequestError<SbrmiError>> {
        self.rdmsr::<u16>(thread, msr)
    }

    fn rdmsr32(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        msr: u32,
    ) -> Result<u32, RequestError<SbrmiError>> {
        self.rdmsr::<u32>(thread, msr)
    }

    fn rdmsr64(
        &mut self,
        _: &RecvMessage,
        thread: u32,
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

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let devs = i2c_config::devices::sbrmi(I2C.get_task_id());
    let sbrmi = loop {
        match sbrmi::SbRmi::<ByteProto>::new(devs[0]) {
            Ok(sbrmi) => break Some(sbrmi),
            Err(err) => {
                ringbuf_entry!(Trace::InitializationError(err));
                hl::sleep_for(1_000);
                continue;
            }
        }
    };
    let mut server = ServerImpl { sbrmi };
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use super::SbrmiError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
