// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump Agent

#![no_std]
#![no_main]

use dump_agent_api::*;
use idol_runtime::RequestError;
use ringbuf::*;
use userlib::*;

struct ServerImpl {
    areas: [DumpArea; 3],
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Address(u32),
    Value(u8),
    None,
}

ringbuf!(Trace, 32, Trace::None);

impl idl::InOrderDumpAgentImpl for ServerImpl {
    fn read_dump(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
        offset: u32,
    ) -> Result<[u8; 256], RequestError<DumpAgentError>> {
        let mut rval = [0u8; 256];
        let offset = offset as usize;

        if index as usize >= self.areas.len() {
            Err(DumpAgentError::InvalidArea.into())
        } else {
            let area = &self.areas[index as usize];

            ringbuf_entry!(Trace::Address(area.address));

            let slice = unsafe {
                core::slice::from_raw_parts(
                    area.address as *const _,
                    area.length as usize,
                )
            };

            ringbuf_entry!(Trace::Value(slice[0]));

            if offset + rval.len() > slice.len() {
                return Err(DumpAgentError::BadOffset.into());
            }

            let len = rval.len();

            //
            // For unclear reasons, the compiler (apprently?) gets confused
            // about the alignment of `slice` -- and it ends up trying to
            // do a load from an unaligned address.  So this will fail:
            //
            rval.copy_from_slice(&slice[offset..offset + len]);

            //
            // By contast, this will work:
            //
            // for ndx in offset..offset + len {
            //    ringbuf_entry!(Trace::Value(slice[ndx]));
            //    rval[ndx - offset] = slice[ndx];
            // }

            Ok(rval)
        }
    }

    fn get_dump_areas(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<usize, RequestError<DumpAgentError>> {
        Ok(self.areas.len())
    }

    fn get_dump_area(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
    ) -> Result<DumpArea, RequestError<DumpAgentError>> {
        if index as usize >= self.areas.len() {
            Err(DumpAgentError::InvalidArea.into())
        } else {
            Ok(self.areas[index as usize])
        }
    }

    fn initialize_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        Err(DumpAgentError::InvalidArea.into())
    }

    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        Err(DumpAgentError::InvalidArea.into())
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
        areas: [
            DumpArea {
                address: 0x30020000,
                length: 0x20000,
            },
            DumpArea {
                address: 0x30040000,
                length: 0x8000,
            },
            DumpArea {
                address: 0x38000000,
                length: 0x10000,
            },
        ],
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::DumpAgentError;
    use super::DumpArea;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
