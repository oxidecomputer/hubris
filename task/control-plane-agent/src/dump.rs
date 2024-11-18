// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::mem::size_of;
use dump_agent_api::DumpAgent;
use gateway_messages::{DumpError, DumpSegment, DumpTask, SpError};
use userlib::{task_slot, UnwrapLite};
use zerocopy::{AsBytes, FromBytes};

task_slot!(DUMP_AGENT, dump_agent);

#[derive(Copy, Clone)]
pub struct ClientDumpState {
    /// Key used to distinguish between clients (should be random)
    key: u32,

    /// Index of the dump area being read
    area_index: u8,

    /// Offset into the dump area
    ///
    /// This should point to a `DumpSegmentData` header, followed by compressed
    /// data.
    offset: u32,
}

const MAX_DUMP_CLIENTS: usize = 4;
pub struct DumpState {
    agent: DumpAgent,
    clients: [Option<ClientDumpState>; MAX_DUMP_CLIENTS],
}

impl DumpState {
    pub fn new() -> Self {
        DumpState {
            clients: [None; MAX_DUMP_CLIENTS],
            agent: DumpAgent::from(DUMP_AGENT.get_task_id()),
        }
    }

    pub(crate) fn get_task_dump_count(&mut self) -> Result<u32, SpError> {
        let mut count = 0;
        for index in 0.. {
            // XXX accidentally quadratic!
            let data = self
                .agent
                .read_dump(index, 0)
                .map_err(|_e| SpError::Dump(DumpError::BadArea))?;
            let header = humpty::DumpAreaHeader::read_from(
                &data[..size_of::<humpty::DumpAreaHeader>()],
            )
            .unwrap_lite();
            if header.contents == humpty::DumpContents::SingleTask.into()
                && header.nsegments > 0
            {
                count += 1;
            }
            if header.next == 0 {
                break;
            }
        }
        Ok(count)
    }

    pub(crate) fn task_dump_read_start(
        &mut self,
        dump_index: u32,
        key: u32,
    ) -> Result<DumpTask, SpError> {
        // Find the area where this dump starts
        let mut count = 0;
        let mut found = None;
        let mut data = [0u8; 256];
        let mut header = humpty::DumpAreaHeader::new_zeroed();
        for index in 0.. {
            // XXX accidentally quadratic!
            data = self
                .agent
                .read_dump(index, 0)
                .map_err(|_e| SpError::Dump(DumpError::BadArea))?;
            header = humpty::DumpAreaHeader::read_from(
                &data[..size_of::<humpty::DumpAreaHeader>()],
            )
            .unwrap_lite();
            if header.contents == humpty::DumpContents::SingleTask.into()
                && header.nsegments > 0
            {
                if count == dump_index {
                    found = Some(index);
                    break;
                } else {
                    count += 1;
                }
            }
            if header.next == 0 {
                break;
            }
        }
        let Some(index) = found else {
            return Err(SpError::Dump(DumpError::BadIndex));
        };

        // Here we go!
        let offset = size_of::<humpty::DumpAreaHeader>()
            + usize::from(header.nsegments)
                * size_of::<humpty::DumpSegmentHeader>();
        let task = humpty::DumpTask::read_from_prefix(&data[offset..])
            .ok_or(SpError::Dump(DumpError::NoDumpTaskHeader))?;
        if task.magic != humpty::DUMP_TASK_MAGIC {
            return Err(SpError::Dump(DumpError::CorruptTaskHeader));
        }

        // Pick a client slot to use.  Prefer empty slots or slots that used the
        // same key (which is bad behavior on the client's part); otherwise,
        // just use slot 0.
        let slot = self
            .clients
            .iter()
            .position(|c| c.map(|c| c.key == key).unwrap_or(true))
            .unwrap_or(0);
        self.clients[slot] = Some(ClientDumpState {
            key,
            area_index: index,
            offset: (offset + size_of::<humpty::DumpTask>()) as u32,
        });

        Ok(DumpTask {
            task: task.id,
            time: task.time,
        })
    }

    fn clear_client_state(&mut self, key: u32) {
        self.clients
            .iter_mut()
            .filter(|c| c.map(|c| c.key == key).unwrap_or(false))
            .for_each(|c| *c = None)
    }

    pub(crate) fn task_dump_read_continue(
        &mut self,
        key: u32,
        buf: &mut [u8],
    ) -> Result<Option<DumpSegment>, SpError> {
        let Some(state) =
            self.clients.iter_mut().flatten().find(|c| c.key == key)
        else {
            return Err(SpError::Dump(DumpError::BadKey));
        };

        let mut header = humpty::DumpAreaHeader::new_zeroed();
        self.agent
            .read_dump_into(state.area_index, 0, header.as_bytes_mut())
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;

        // Make sure the header is still valid
        if header.contents != humpty::DumpContents::SingleTask.into() {
            return Err(SpError::Dump(DumpError::NoLongerValid));
        }

        // Move along to the next area if we're at the end
        if state.offset + size_of::<humpty::DumpSegmentData>() as u32
            > header.written
        {
            if header.next == 0 {
                // we're done, because there's no more dump areas
                self.clear_client_state(key);
                return Ok(None);
            }

            // Move to the next area and read the header
            state.area_index += 1;
            self.agent
                .read_dump_into(state.area_index, 0, header.as_bytes_mut())
                .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;

            // If the next header is of a different type, then we have no more
            // data left to read and can return None.
            if header.contents != humpty::DumpContents::SingleTask.into()
                || header.nsegments != 0
            {
                self.clear_client_state(key);
                return Ok(None);
            }
            // Skip the area header
            state.offset = size_of::<humpty::DumpAreaHeader>() as u32;
        }

        // Read the dump segment data header
        let mut ds = humpty::DumpSegmentData::new_zeroed();
        self.agent
            .read_dump_into(state.area_index, state.offset, ds.as_bytes_mut())
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;
        state.offset += size_of::<humpty::DumpSegmentData>() as u32;

        // Read the compressed bytes directly into the tx buffer
        if ds.compressed_length as usize > buf.len() {
            return Err(SpError::Dump(DumpError::SegmentTooLong));
        }
        self.agent
            .read_dump_into(
                state.area_index,
                state.offset,
                &mut buf[..ds.compressed_length as usize],
            )
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;
        state.offset += ds.compressed_length as u32;
        while state.offset & 3 != 0 {
            // pad to the nearest u32
            // XXX why is `humpty::DUMP_SEGMENT_MASK` private?
            state.offset += 1;
        }

        Ok(Some(DumpSegment {
            address: ds.address,
            compressed_length: ds.compressed_length,
            uncompressed_length: ds.uncompressed_length,
        }))
    }
}
