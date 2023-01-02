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
use core::mem::size_of;
use static_assertions::const_assert;

//
// Our DUMP_READ_SIZE must be an even power of 2 -- and practically speaking
// cannot be more than 1K
//
const_assert!(DUMP_READ_SIZE & (DUMP_READ_SIZE - 1) == 0);
const_assert!(DUMP_READ_SIZE <= 1024);

struct ServerImpl {
    areas: [DumpArea; 3],
}

impl ServerImpl {
    fn area(&self, mut offset: u32) -> Result<&[u8], DumpAgentError> {
        for area in &self.areas {
            if offset < area.length {
                let addr = (area.address + offset) as *const u8;
                let len = (area.length - offset) as usize;

                return Ok(unsafe { core::slice::from_raw_parts(addr, len) });
            }

            offset -= area.length;
        }
 
        Err(DumpAgentError::BadOffset)
    }

    fn initialize(&self) {
        for area in &self.areas {
            unsafe {
                let header = area.address as *mut DumpAreaHeader;

                (*header).nsegments = 0;
                (*header).written = size_of::<DumpAreaHeader>() as u32;
                (*header).length = area.length;
                (*header).agent_version = DUMP_AGENT_VERSION;
                (*header).magic = DUMP_MAGIC;
            }
        }
    }

    fn add_dump_segment(&mut self, addr: u32, length: u32) {
        let area = self.areas[0];

        unsafe {
            let header = area.address as *mut DumpAreaHeader;

            if (*header).magic != DUMP_MAGIC {
                panic!("bad dump magic!");
            }

            let nsegments = (*header).nsegments;

            let offset = size_of::<DumpAreaHeader>() +
               (nsegments as usize) * size_of::<DumpSegmentHeader>();

            let saddr = area.address as usize + offset;
            let segment = saddr as *mut DumpSegmentHeader;
 
            (*segment).address = addr;
            (*segment).length = length;

            (*header).nsegments = nsegments + 1;
            (*header).written = (offset + size_of::<DumpSegmentHeader>()) as u32;
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Address(u32),
    Value(u8),
    None,
}

ringbuf!(Trace, 32, Trace::None);

impl idl::InOrderDumpAgentImpl for ServerImpl {
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
        self.initialize();
        Ok(())
    }

    fn add_dump_segment(
        &mut self,
        _msg: &RecvMessage,
        address: u32,
        length: u32,
    ) -> Result<(), RequestError<DumpAgentError>> {

        if address & 0b111 != 0 {
            return Err(DumpAgentError::UnalignedSegmentAddress.into());
        }

        if (length as usize) & (DUMP_READ_SIZE - 1) != 0 {
            return Err(DumpAgentError::UnalignedSegmentLength.into());
        }

        self.add_dump_segment(address, length);
        Ok(())
    }

    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        Err(DumpAgentError::InvalidArea.into())
    }

    fn read_dump(
        &mut self,
        _msg: &RecvMessage,
        offset: u32,
    ) -> Result<[u8; DUMP_READ_SIZE], RequestError<DumpAgentError>> {
        let mut rval = [0u8; DUMP_READ_SIZE];

        if offset & ((rval.len() as u32) - 1) != 0 {
            return Err(DumpAgentError::UnalignedOffset.into());
        }

        let area = self.area(offset)?;

        for ndx in 0..rval.len() {
            rval[ndx] = area[ndx];
        }

        Ok(rval)
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

    server.initialize();

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
