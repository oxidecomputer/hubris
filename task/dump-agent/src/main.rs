// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump Agent

#![no_std]
#![no_main]

use dump_agent_api::*;
use idol_runtime::RequestError;
#[cfg(not(feature = "no-rot"))]
use ringbuf::*;
use static_assertions::const_assert;
use task_jefe_api::Jefe;
use userlib::*;

#[cfg(feature = "net")]
mod udp;

//
// Our DUMP_READ_SIZE must be an even power of 2 -- and practically speaking
// cannot be more than 1K
//
const_assert!(DUMP_READ_SIZE & (DUMP_READ_SIZE - 1) == 0);
const_assert!(DUMP_READ_SIZE <= 1024);

struct ServerImpl {
    jefe: Jefe,
    #[cfg(feature = "net")]
    net: task_net_api::Net,
}

#[cfg(not(feature = "no-rot"))]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    SpRotDump,
    SpRotDumpResult(Result<(), DumpAgentError>),
    None,
}

#[cfg(not(feature = "no-rot"))]
ringbuf!(Trace, 4, Trace::None);

#[cfg(not(feature = "no-rot"))]
task_slot!(SPROT, sprot);

task_slot!(JEFE, jefe);

impl ServerImpl {
    fn initialize(&self) -> Result<(), DumpAgentError> {
        self.jefe.reinitialize_dump_areas()
    }

    fn dump_area(&self, index: u8) -> Result<DumpArea, DumpAgentError> {
        self.jefe.get_dump_area(index)
    }

    fn claim_dump_area(&self) -> Result<DumpArea, DumpAgentError> {
        self.jefe.claim_dump_area()
    }

    fn add_dump_segment(
        &mut self,
        addr: u32,
        length: u32,
    ) -> Result<(), DumpAgentError> {
        if addr & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentAddress);
        }

        if (length as usize) & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentLength);
        }

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
            area.region.address,
            addr,
            length,
            |addr, buf, _| unsafe { humpty::from_mem(addr, buf) },
            |addr, buf| unsafe { humpty::to_mem(addr, buf) },
        )
        .map_err(|_| DumpAgentError::BadSegmentAdd)
    }

    fn read_dump<'b, B: idol_runtime::BufWriter<'b>>(
        &mut self,
        index: u8,
        offset: u32,
        mut rval: B,
    ) -> Result<usize, DumpAgentError> {
        let offset = offset as usize;
        let read_size = rval.remaining_size();

        let area = self.dump_area(index)?;

        let written = unsafe {
            let header = area.region.address as *mut DumpAreaHeader;
            core::ptr::read_volatile(header).written
        } as usize;

        if written > offset {
            let base = area.region.address as *const u8;
            let base = unsafe { base.add(offset) };

            let read_count = (written - offset).min(read_size);
            for i in 0..read_count {
                rval.write(unsafe { core::ptr::read_volatile(base.add(i)) })
                    .map_err(|_| DumpAgentError::LeaseWriteFailed)?;
            }

            Ok(read_count)
        } else {
            Err(DumpAgentError::BadOffset)
        }
    }

    fn dump_task(&mut self, task_index: u32) -> Result<u8, DumpAgentError> {
        let out = self.jefe.dump_task(task_index)?;
        Ok(out)
    }

    fn dump_task_region(
        &mut self,
        task_index: u32,
        start: u32,
        length: u32,
    ) -> Result<u8, DumpAgentError> {
        let out = self.jefe.dump_task_region(task_index, start, length)?;
        Ok(out)
    }

    fn reinitialize_dump_from(
        &mut self,
        index: u8,
    ) -> Result<(), DumpAgentError> {
        self.jefe.reinitialize_dump_from(index)?;
        Ok(())
    }

    #[cfg(not(feature = "no-rot"))]
    fn take_dump(&mut self) -> Result<(), DumpAgentError> {
        use drv_sprot_api::DumpOrSprotError;

        let sprot = drv_sprot_api::SpRot::from(SPROT.get_task_id());

        ringbuf_entry!(Trace::SpRotDump);

        let area = self.dump_area(0)?;
        if area.contents != humpty::DumpContents::WholeSystem {
            return Err(DumpAgentError::UnclaimedDumpArea);
        }

        let res = match sprot.dump(area.region.address) {
            Err(DumpOrSprotError::Dump(e)) => Err(e.into()),
            Err(_) => Err(DumpAgentError::DumpMessageFailed),
            Ok(()) => Ok(()),
        };

        ringbuf_entry!(Trace::SpRotDumpResult(res));
        res
    }

    #[cfg(feature = "no-rot")]
    fn take_dump(&mut self) -> Result<(), DumpAgentError> {
        Err(DumpAgentError::NotSupported)
    }
}

#[cfg(feature = "net")]
impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::SOCKET_MASK
    }
    fn handle_notification(&mut self, bits: u32) {
        if (bits & notifications::SOCKET_MASK) != 0 {
            // Nothing to do here; we'll handle it in the main loop
        }
    }
}

// If we are not built with net support, we expect no notifications.
#[cfg(not(feature = "net"))]
impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }
    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
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
        self.add_dump_segment(address, length).map_err(|e| e.into())
    }

    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        self.take_dump().map_err(|e| e.into())
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
        let mut out = [0u8; DUMP_READ_SIZE];
        if offset & (out.len() as u32 - 1) != 0 {
            return Err(DumpAgentError::UnalignedOffset.into());
        }
        self.read_dump(index, offset, out.as_mut_slice())
            .map_err(RequestError::from)?;
        Ok(out)
    }

    fn read_dump_into(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
        offset: u32,
        out: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<DumpAgentError>> {
        self.read_dump(
            index,
            offset,
            idol_runtime::LeaseBufWriter::<_, 16>::from(out),
        )
        .map_err(|e| e.into())
    }

    fn dump_task(
        &mut self,
        _msg: &RecvMessage,
        task_index: u32,
    ) -> Result<u8, RequestError<DumpAgentError>> {
        self.dump_task(task_index).map_err(|e| e.into())
    }

    fn dump_task_region(
        &mut self,
        _msg: &RecvMessage,
        task_index: u32,
        start: u32,
        length: u32,
    ) -> Result<u8, RequestError<DumpAgentError>> {
        self.dump_task_region(task_index, start, length)
            .map_err(|e| e.into())
    }

    fn reinitialize_dump_from(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
    ) -> Result<(), RequestError<DumpAgentError>> {
        self.reinitialize_dump_from(index).map_err(|e| e.into())
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    #[cfg(feature = "net")]
    {
        task_slot!(NET, net);
        let (rx_data_buf, tx_data_buf) = udp::claim_statics();
        let mut server = ServerImpl {
            jefe: Jefe::from(JEFE.get_task_id()),
            net: task_net_api::Net::from(NET.get_task_id()),
        };

        loop {
            server.check_net(
                rx_data_buf.as_mut_slice(),
                tx_data_buf.as_mut_slice(),
            );
            idol_runtime::dispatch(&mut buffer, &mut server);
        }
    }

    #[cfg(not(feature = "net"))]
    {
        let mut server = ServerImpl {
            jefe: Jefe::from(JEFE.get_task_id()),
        };
        loop {
            idol_runtime::dispatch(&mut buffer, &mut server);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
