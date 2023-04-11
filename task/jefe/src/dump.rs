// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump support for Jefe

use humpty::{DumpArea, DumpContents};
use ringbuf::*;
use task_jefe_api::DumpAgentError;
use userlib::*;

#[derive(Debug, Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initialized,
    GetDumpArea(u8),
    Base(u32),
    GetDumpAreaFailed(humpty::DumpError<()>),
    ClaimDumpAreaFailed(humpty::DumpError<()>),
    Claiming,
    Dumping(usize, u32),
    DumpArea(Result<Option<DumpArea>, humpty::DumpError<()>>),
    DumpRegion(abi::TaskDumpRegion),
    DumpRegionsFailed(humpty::DumpError<()>),
    DumpStart(u32),
    DumpReading(u32, usize, bool),
    DumpRead(usize),
    DumpDone(Result<(), humpty::DumpError<()>>),
}

ringbuf!(Trace, 8, Trace::None);

pub fn initialize_dump_areas() -> u32 {
    let areas = humpty::initialize_dump_areas(
        &crate::generated::DUMP_AREAS,
        Some(0x1000),
    )
    .unwrap_lite();

    ringbuf_entry!(Trace::Initialized);

    areas
}

pub fn get_dump_area(base: u32, index: u8) -> Result<DumpArea, DumpAgentError> {
    ringbuf_entry!(Trace::GetDumpArea(index));
    ringbuf_entry!(Trace::Base(base));

    match humpty::get_dump_area(base, index, |addr, buf, _| unsafe {
        humpty::from_mem(addr, buf)
    }) {
        Err(e) => {
            ringbuf_entry!(Trace::GetDumpAreaFailed(e));
            Err(DumpAgentError::InvalidArea)
        }

        Ok(rval) => Ok(rval),
    }
}

pub fn claim_dump_area(base: u32) -> Result<DumpArea, DumpAgentError> {
    ringbuf_entry!(Trace::Claiming);
    match humpty::claim_dump_area(
        base,
        DumpContents::WholeSystem,
        |addr, buf, _| unsafe { humpty::from_mem(addr, buf) },
        |addr, buf| unsafe { humpty::to_mem(addr, buf) },
    ) {
        Err(e) => {
            ringbuf_entry!(Trace::ClaimDumpAreaFailed(e));
            Err(DumpAgentError::CannotClaimDumpArea)
        }
        Ok(None) => Err(DumpAgentError::DumpAreaInUse),
        Ok(Some(rval)) => Ok(rval),
    }
}

pub fn dump_task(base: u32, task: usize) -> Result<(), DumpAgentError> {
    ringbuf_entry!(Trace::Dumping(task, base));

    //
    // We need to claim a dump area.  Once it's claimed, we have committed
    // to dumping into it:  any failure will result in a partial or otherwise
    // corrupted dump.
    //
    let area = humpty::claim_dump_area(
        base,
        DumpContents::SingleTask,
        |addr, buf, _| unsafe { humpty::from_mem(addr, buf) },
        |addr, buf| unsafe { humpty::to_mem(addr, buf) },
    );
    ringbuf_entry!(Trace::DumpArea(area));

    let base = match area {
        Ok(Some(area)) => area.address,
        Ok(None) => return Err(DumpAgentError::DumpAreaInUse),
        Err(_) => return Err(DumpAgentError::CannotClaimDumpArea),
    };

    let mut ndx = 0;

    loop {
        //
        // We need to ask the kernel which regions we should dump for this
        // task, which we do by asking for each dump region by index.  Note
        // that get_task_dump_region is O(#regions) -- which makes this loop
        // quadratic: O(#regions * #dumpable).  Fortunately, these numbers are
        // very small: the number of regions is generally 3 or less (and -- as
        // of this writing -- tops out at 7), and the number of dumpable
        // regions is generally just one (two when including the task TCB, but
        // that's constant time to extract).  So this isn't as bad as it
        // potentially looks (and boils down to two iterations over all
        // regions in a task) -- but could become so if these numbers become
        // larger.
        //
        match kipc::get_task_dump_region(task, ndx) {
            None => break,
            Some(region) => {
                ringbuf_entry!(Trace::DumpRegion(region));

                if let Err(e) = humpty::add_dump_segment_header(
                    base,
                    region.base,
                    region.size,
                    |addr, buf, _| unsafe { humpty::from_mem(addr, buf) },
                    |addr, buf| unsafe { humpty::to_mem(addr, buf) },
                ) {
                    ringbuf_entry!(Trace::DumpRegionsFailed(e));
                    return Err(DumpAgentError::BadSegmentAdd);
                }

                ndx += 1;
            }
        }
    }

    ringbuf_entry!(Trace::DumpStart(base));

    //
    // The humpty dance is your chance... to do the dump!
    //
    let r = humpty::dump::<(), 512, { humpty::DUMPER_JEFE }>(
        base,
        Some(humpty::DumpTask::new(task as u16, sys_get_timer().now)),
        || Ok(None),
        |addr, buf, meta| {
            ringbuf_entry!(Trace::DumpReading(addr, buf.len(), meta));

            //
            // If meta is set, this read is metadata from within the dump
            // regions (e.g., a dump header or dump segment header), and we'll
            // read it directly -- otherwise, we'll ask the kernel.
            //
            if meta {
                unsafe { humpty::from_mem(addr, buf) }
            } else {
                let r = kipc::read_task_dump_region(
                    task,
                    TaskDumpRegion {
                        base: addr,
                        size: buf.len() as u32,
                    },
                    buf,
                );

                ringbuf_entry!(Trace::DumpRead(r));
                Ok(())
            }
        },
        |addr, buf| unsafe { humpty::to_mem(addr, buf) },
    );

    ringbuf_entry!(Trace::DumpDone(r));
    r.map_err(|_| DumpAgentError::DumpFailed)
}
