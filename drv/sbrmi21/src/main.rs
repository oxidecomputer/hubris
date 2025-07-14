// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Task for communicating to the host CPU via the SB-RMI interface.

#![no_std]
#![no_main]

use apml_rs::SbRmi21MailboxCmd;
use drv_i2c_devices::sbrmi21::{self, BlockProto, ByteProto, CpuidResult};
use drv_sbrmi21_api::SbRmi21Error;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{hl, task_slot, RecvMessage};
use zerocopy::{FromBytes, Immutable, KnownLayout};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

struct ServerImpl {
    sbrmi: Option<sbrmi21::SbRmi<ByteProto>>,
}

task_slot!(I2C, i2c_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    InitializationError(SbRmi21Error),
    CpuidError(SbRmi21Error),
    CpuidResult(CpuidResult),
    Rdmsr(u32),
    RdmsrError(SbRmi21Error),
    RdmsrOk,
}

ringbuf!(Trace, 16, Trace::None);

impl ServerImpl {
    fn with_sbrmi_block_proto<T, F>(
        &mut self,
        thunk: F,
    ) -> Result<T, SbRmi21Error>
    where
        F: FnOnce(&sbrmi21::SbRmi<BlockProto>) -> Result<T, SbRmi21Error>,
    {
        let sbrmi = self.sbrmi.take().ok_or(SbRmi21Error::Unavailable)?;
        let block = sbrmi.into_block_proto()?;
        let res = thunk(&block);
        self.sbrmi = Some(block.into_byte_proto()?);
        res.into()
    }

    fn with_sbrmi_byte_proto<T, F>(
        &mut self,
        thunk: F,
    ) -> Result<T, SbRmi21Error>
    where
        F: FnOnce(&sbrmi21::SbRmi<ByteProto>) -> Result<T, SbRmi21Error>,
    {
        let sbrmi = self.sbrmi.take().ok_or(SbRmi21Error::Unavailable)?;
        let res = thunk(&sbrmi);
        self.sbrmi = Some(sbrmi);
        res
    }

    fn rdmsr<T: FromBytes + Immutable + KnownLayout>(
        &mut self,
        thread: u32,
        msr: u32,
    ) -> Result<T, RequestError<SbRmi21Error>> {
        ringbuf_entry!(Trace::Rdmsr(msr));
        match self.with_sbrmi_block_proto(|sbrmi| sbrmi.rdmsr(thread, msr)) {
            Err(code) => {
                ringbuf_entry!(Trace::RdmsrError(code));
                Err(SbRmi21Error::from(code).into())
            }
            Ok(rval) => {
                ringbuf_entry!(Trace::RdmsrOk);
                Ok(rval)
            }
        }
    }
}

impl idl::InOrderSbRmi21Impl for ServerImpl {
    fn nthreads(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<SbRmi21Error>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi21::SbRmi::<ByteProto>::nthreads)?)
    }

    fn enabled(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 32], RequestError<SbRmi21Error>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi21::SbRmi::<ByteProto>::enabled)?)
    }

    fn alert(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 32], RequestError<SbRmi21Error>> {
        Ok(self.with_sbrmi_byte_proto(sbrmi21::SbRmi::<ByteProto>::alert)?)
    }

    fn mailbox(
        &mut self,
        _: &RecvMessage,
        cmd: [u8; 32],
    ) -> Result<Option<u32>, RequestError<SbRmi21Error>> {
        let (cmd, _) = hubpack::deserialize::<SbRmi21MailboxCmd>(&cmd[..])
            .map_err(|_| SbRmi21Error::BadMailboxCmd)?;
        Ok(self.with_sbrmi_byte_proto(|sbrmi| sbrmi.mailbox(cmd))?)
    }

    fn cpuid(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        eax: u32,
        ecx: u32,
    ) -> Result<[u32; 4], RequestError<SbRmi21Error>> {
        match self.with_sbrmi_block_proto(|sbrmi| sbrmi.cpuid(thread, eax, ecx))
        {
            Err(code) => {
                ringbuf_entry!(Trace::CpuidError(code));
                let err = SbRmi21Error::from(code);
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
    ) -> Result<u8, RequestError<SbRmi21Error>> {
        self.rdmsr::<u8>(thread, msr)
    }

    fn rdmsr16(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        msr: u32,
    ) -> Result<u16, RequestError<SbRmi21Error>> {
        self.rdmsr::<u16>(thread, msr)
    }

    fn rdmsr32(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        msr: u32,
    ) -> Result<u32, RequestError<SbRmi21Error>> {
        self.rdmsr::<u32>(thread, msr)
    }

    fn rdmsr64(
        &mut self,
        _: &RecvMessage,
        thread: u32,
        msr: u32,
    ) -> Result<u64, RequestError<SbRmi21Error>> {
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
    let devs = i2c_config::devices::sbrmi21(I2C.get_task_id());
    let sbrmi = loop {
        match sbrmi21::SbRmi::<ByteProto>::new(devs[0]) {
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
    use super::SbRmi21Error;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
