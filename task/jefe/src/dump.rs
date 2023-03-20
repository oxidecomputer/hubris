// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump support for Jefe

use humpty::{DumpAgent, DumpArea};
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
    Dumping(usize, Option<u32>),
    DumpArea(Result<Option<DumpArea>, humpty::DumpError<()>>),
    DumpRegion(abi::TaskDumpRegion),
    DumpRegionsFailed(humpty::DumpError<()>),
    DumpStart(u32),
    DumpReading(u32, usize, bool),
    DumpRead(usize),
    DumpDone(Result<(), humpty::DumpError<()>>),
}

ringbuf!(Trace, 8, Trace::None);

pub fn initialize_dump_areas() -> Option<u32> {
    let areas = humpty::initialize_dump_areas(
        &crate::generated::DUMP_AREAS,
        Some(0x1000),
    );

    ringbuf_entry!(Trace::Initialized);

    areas
}

pub fn get_dump_area(
    base: Option<u32>,
    index: u8,
) -> Result<DumpArea, DumpAgentError> {
    ringbuf_entry!(Trace::GetDumpArea(index));

    if let Some(base) = base {
        ringbuf_entry!(Trace::Base(base));

        match humpty::get_dump_area(base, index, humpty::from_mem) {
            Err(e) => {
                ringbuf_entry!(Trace::GetDumpAreaFailed(e));
                Err(DumpAgentError::InvalidArea)
            }

            Ok(rval) => Ok(rval),
        }
    } else {
        Err(DumpAgentError::NoDumpAreas)
    }
}

pub fn claim_dump_area(base: Option<u32>) -> Result<DumpArea, DumpAgentError> {
    if let Some(base) = base {
        ringbuf_entry!(Trace::Claiming);
        match humpty::claim_dump_area(
            base,
            DumpAgent::Task,
            humpty::from_mem,
            humpty::to_mem,
        ) {
            Err(e) => {
                ringbuf_entry!(Trace::ClaimDumpAreaFailed(e));
                Err(DumpAgentError::CannotClaimDumpArea)
            }

            Ok(None) => Err(DumpAgentError::DumpAreaInUse),

            Ok(Some(rval)) => Ok(rval),
        }
    } else {
        Err(DumpAgentError::NoDumpAreas)
    }
}

pub fn dump_task(base: Option<u32>, task: usize) {
    ringbuf_entry!(Trace::Dumping(task, base));

    let base = match base {
        Some(base) => base,
        None => return,
    };

    //
    // We need to claim a dump area.  Once it's claimed, we have committed
    // to dumping into it:  any failure will result in a partial or otherwise
    // corrupted dump.
    //
    let area = humpty::claim_dump_area(
        base,
        DumpAgent::Jefe,
        humpty::from_mem,
        humpty::to_mem,
    );

    ringbuf_entry!(Trace::DumpArea(area));

    let base = match area {
        Ok(Some(area)) => area.address,
        _ => return,
    };

    let mut ndx = 0;

    loop {
        match kipc::get_task_dump_region(task, ndx) {
            None => break,
            Some(region) => {
                ringbuf_entry!(Trace::DumpRegion(region));

                if let Err(e) = humpty::add_dump_segment(
                    base,
                    region.base,
                    region.size,
                    humpty::from_mem,
                    humpty::to_mem,
                ) {
                    ringbuf_entry!(Trace::DumpRegionsFailed(e));
                    return;
                }

                ndx += 1;
            }
        }
    }

    ringbuf_entry!(Trace::DumpStart(base));

    let r = humpty::dump::<(), 512, { humpty::DUMPER_JEFE }>(
        base,
        Some(humpty::DumpTask::new(task as u16, sys_get_timer().now)),
        || Ok(None),
        |addr, buf, meta| {
            ringbuf_entry!(Trace::DumpReading(addr, buf.len(), meta));
            if meta {
                humpty::from_mem(addr, buf, meta)
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
        humpty::to_mem,
    );

    ringbuf_entry!(Trace::DumpDone(r));
}
