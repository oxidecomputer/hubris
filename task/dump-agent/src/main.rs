// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump Agent

#![no_std]
#![no_main]

use dump_agent_api::*;
use idol_runtime::RequestError;
use static_assertions::const_assert;
use task_jefe_api::Jefe;
use userlib::*;

//
// Our DUMP_READ_SIZE must be an even power of 2 -- and practically speaking
// cannot be more than 1K
//
const_assert!(DUMP_READ_SIZE & (DUMP_READ_SIZE - 1) == 0);
const_assert!(DUMP_READ_SIZE <= 1024);

struct ServerImpl;

#[cfg(not(feature = "no-rot"))]
task_slot!(SPROT, sprot);

task_slot!(JEFE, jefe);

impl ServerImpl {
    fn initialize(&self) -> Result<(), DumpAgentError> {
        let jefe = Jefe::from(JEFE.get_task_id());
        jefe.initialize_dump_areas()
    }

    fn dump_area(&self, index: u8) -> Result<DumpArea, DumpAgentError> {
        let jefe = Jefe::from(JEFE.get_task_id());
        jefe.get_dump_area(index)
    }

    fn claim_dump_area(&self) -> Result<DumpArea, DumpAgentError> {
        let jefe = Jefe::from(JEFE.get_task_id());
        jefe.claim_dump_area()
    }

    fn add_dump_segment(
        &mut self,
        addr: u32,
        length: u32,
    ) -> Result<(), DumpAgentError> {
        let area = self.dump_area(0)?;

        //
        // If we haven't already claimed this area for purposes of dumping the
        // entire system, we need to do so first. Claiming this area for
        // [`DumpContents::WholeSystem`] will claim all dump areas or fail if
        // any are unavailable.  (If we have already claimed this area, then
        // we are here because we are adding a subsequent segment to dump.)
        //
        if area.contents != humpty::DumpContents::WholeSystem {
            self.claim_dump_area()?;
        }

        humpty::add_dump_segment_header(
            area.address,
            addr,
            length,
            humpty::from_mem,
            humpty::to_mem,
        )
        .map_err(|_| DumpAgentError::BadSegmentAdd)
    }
}

impl idl::InOrderDumpAgentImpl for ServerImpl {
    fn get_dump_area(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
    ) -> Result<DumpArea, RequestError<DumpAgentError>> {
        self.dump_area(index).map_err(|e| e.into())
    }

    fn initialize_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        self.initialize().map_err(|e| e.into())
    }

    fn add_dump_segment(
        &mut self,
        _msg: &RecvMessage,
        address: u32,
        length: u32,
    ) -> Result<(), RequestError<DumpAgentError>> {
        if address & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentAddress.into());
        }

        if (length as usize) & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentLength.into());
        }

        self.add_dump_segment(address, length)?;

        Ok(())
    }

    #[cfg(not(feature = "no-rot"))]
    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        let sprot = drv_sprot_api::SpRot::from(SPROT.get_task_id());
        let mut buf = [0u8; 4];

        let area = self.dump_area(0)?;

        if area.contents != humpty::DumpContents::WholeSystem {
            return Err(DumpAgentError::UnclaimedDumpArea.into());
        }

        match sprot.send_recv(
            drv_sprot_api::MsgType::DumpReq,
            &area.address.to_le_bytes(),
            &mut buf,
        ) {
            Err(_) => Err(DumpAgentError::DumpFailed.into()),
            Ok(_) => Ok(()),
        }
    }

    #[cfg(feature = "no-rot")]
    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        Err(DumpAgentError::NotSupported.into())
    }

    //
    // We return a buffer of fixed size here instead of taking a lease
    // because we want/need this to work with consumers who are not
    // lease aware (specifically, udprpc and hiffy).
    //
    fn read_dump(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
        offset: u32,
    ) -> Result<[u8; DUMP_READ_SIZE], RequestError<DumpAgentError>> {
        let mut rval = [0u8; DUMP_READ_SIZE];

        if offset & ((rval.len() as u32) - 1) != 0 {
            return Err(DumpAgentError::UnalignedOffset.into());
        }

        let area = self.dump_area(index)?;

        let written = unsafe {
            let header = area.address as *mut DumpAreaHeader;
            core::ptr::read_volatile(header).written
        };

        if written > offset {
            let to_read = written - offset;
            let base = area.address as *const u8;
            let base = unsafe { base.add(offset as usize) };

            for i in 0..usize::min(to_read as usize, DUMP_READ_SIZE) {
                rval[i] = unsafe { core::ptr::read_volatile(base.add(i)) };
            }

            Ok(rval)
        } else {
            Err(DumpAgentError::BadOffset.into())
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl;
    server.initialize().unwrap_lite();

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
