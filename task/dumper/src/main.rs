// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dumper

#![no_std]
#![no_main]

use drv_sp_ctrl_api::{SpCtrl, SpCtrlError};
use dumper_api::*;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use userlib::*;
use zerocopy::FromBytes;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    DumpInitiated(u32),
    SetupFailed(SpCtrlError),
    DumpHeader([u8; 4]),
    Reading(u32, usize, usize),
    Writing(u32, usize, usize),
    Done(Result<(), humpty::DumpError<DumperError>>),
    SetupDone,
    DataWriteFailed(SpCtrlError),
    DataReadFailed,
    ReadingRegister(u16),
    RegisterReadFailed(SpCtrlError),
    Halted,
    Resumed,
    ResumeFailed,
    ReinitFailed,
    ReinitSucceededButResumeFailed,
    ReinitResumed,
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

        if let Err(err) = sp_ctrl.setup() {
            ringbuf_entry!(Trace::SetupFailed(err));
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
        if sp_ctrl.halt().is_err() {
            return Err(DumperError::FailedToHalt.into());
        }

        ringbuf_entry!(Trace::Halted);

        let mut nread = 0;
        let mut nwritten = 0;
        let mut reg = 0;

        //
        // Now dump, with a buffer size of 1024.  (1024 is a sweet spot with
        // respect to overhead of header size vs. loss to orphaned space at
        // the end of segments.)
        //
        let r = humpty::dump::<DumperError, 1024, { humpty::DUMPER_EXTERNAL }>(
            header.address,
            None,
            || {
                //
                // Dump the next valid register, starting where we last left off.
                //
                let start = reg;

                for r in start..=31 {
                    ringbuf_entry!(Trace::ReadingRegister(r));
                    match sp_ctrl.read_core_register(r) {
                        Ok(val) => {
                            reg = r + 1;
                            return Ok(Some(humpty::RegisterRead(r, val)));
                        }
                        Err(SpCtrlError::InvalidCoreRegister) => {}
                        Err(e) => {
                            ringbuf_entry!(Trace::RegisterReadFailed(e));
                            return Err(DumperError::RegisterReadFailed);
                        }
                    }
                }
                Ok(None)
            },
            |addr, buf, _meta| {
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

        if sp_ctrl.resume().is_err() {
            ringbuf_entry!(Trace::ResumeFailed);

            //
            // This is bad: we have failed to resume a stopped SP.  We really
            // (really!) don't want to leave the SP stopped, so we'll attempt
            // to reinitialize and re-resume.  (Experience has indicated that
            // when this occurs -- and it has been seen to occur as ~15%
            // of the time on the bench! -- reinitialization is always
            // sufficient to allow for us to resume the SP.)
            //
            if sp_ctrl.setup().is_err() {
                ringbuf_entry!(Trace::ReinitFailed);
            } else if sp_ctrl.resume().is_err() {
                ringbuf_entry!(Trace::ReinitSucceededButResumeFailed);
            } else {
                ringbuf_entry!(Trace::ReinitResumed);
            }

            if r.is_err() {
                return Err(DumperError::FailedToResumeAfterFailure.into());
            } else {
                return Err(DumperError::FailedToResume.into());
            }
        }

        ringbuf_entry!(Trace::Resumed);
        r.map_err(|_| DumperError::DumpFailed)?;

        Ok(())
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
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
