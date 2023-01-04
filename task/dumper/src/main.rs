// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dumper

#![no_std]
#![no_main]

use drv_sp_ctrl_api::{SpCtrl, SpCtrlError};
use dumper_api::*;
use idol_runtime::RequestError;
use ringbuf::*;
use userlib::*;
use zerocopy::FromBytes;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    DumpInitiated(u32),
    DumpHeader([u8; 4]),
    Reading(u32, usize, usize),
    Writing(u32, usize, usize),
    Done(Result<(), humpty::DumpError<DumperError>>),
    SetupDone,
    DataWriteFailed(SpCtrlError),
    DataReadFailed,
    Halted,
    Resumed,
    None,
}

task_slot!(SP_CTRL, swd);
const READ_SIZE: usize = 256;

ringbuf!(Trace, 16, Trace::None);

struct ServerImpl;

impl idl::InOrderDumperImpl for ServerImpl {
    fn dump(
        &mut self,
        _msg: &RecvMessage,
        addr: u32,
    ) -> Result<(), RequestError<DumperError>> {
        ringbuf_entry!(Trace::DumpInitiated(addr));
        let sp_ctrl = SpCtrl::from(SP_CTRL.get_task_id());

        if sp_ctrl.setup().is_err() {
            return Err(DumperError::SetupFailed.into());
        }

        ringbuf_entry!(Trace::SetupDone);

        let mut buf: [u8; READ_SIZE] = [0; READ_SIZE];

        if addr & 0x3ff != 0 {
            return Err(DumperError::UnalignedAddress.into());
        }

        if sp_ctrl.read(addr, &mut buf).is_err() {
            return Err(DumperError::HeaderReadFailed.into());
        }

        let header = match humpty::DumpAreaHeader::read_from_prefix(&buf[..]) {
            Some(header) => header,
            None => {
                return Err(DumperError::BadDumpAreaHeader.into());
            }
        };

        ringbuf_entry!(Trace::DumpHeader(header.magic));

        //
        // Good night, sweet prince.
        //
        const DHCSR: u32 = 0xE000EDF0;
        const DHCSR_HALT_MAGIC: u32 = 0xa05f_0003;
        const DHCSR_RESUME_MAGIC: u32 = 0xa05f_0000;

        if sp_ctrl
            .write(DHCSR, &DHCSR_HALT_MAGIC.to_le_bytes())
            .is_err()
        {
            return Err(DumperError::FailedToHalt.into());
        }

        ringbuf_entry!(Trace::Halted);

        let mut nread = 0;
        let mut nwritten = 0;

        let r = humpty::dump::<DumperError, 512>(
            header.address,
            |addr, buf| {
                ringbuf_entry!(Trace::Reading(addr, buf.len(), nread));
                nread += buf.len();

                if sp_ctrl.read(addr, buf).is_err() {
                    ringbuf_entry!(Trace::DataReadFailed);
                    Err(DumperError::ReadFailed)
                } else {
                    Ok(())
                }
            },
            |addr, buf| {
                ringbuf_entry!(Trace::Writing(addr, buf.len(), nwritten));
                nwritten += buf.len();

                match sp_ctrl.write(addr, buf) {
                    Err(e) => {
                        ringbuf_entry!(Trace::DataWriteFailed(e));
                        Err(DumperError::WriteFailed)
                    }
                    Ok(_) => Ok(()),
                }
            },
        );

        ringbuf_entry!(Trace::Done(r));

        if sp_ctrl
            .write(DHCSR, &DHCSR_RESUME_MAGIC.to_le_bytes())
            .is_err()
        {
            return Err(DumperError::FailedToResume.into());
        }

        ringbuf_entry!(Trace::Resumed);

        Ok(())
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
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
