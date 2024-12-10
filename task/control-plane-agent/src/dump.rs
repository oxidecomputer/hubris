// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::mem::size_of;
use dump_agent_api::DumpAgent;
use gateway_messages::{
    DumpCompression, DumpError, DumpSegment, DumpTask, SpError,
};
use userlib::{task_slot, UnwrapLite};
use zerocopy::{AsBytes, FromBytes};

task_slot!(DUMP_AGENT, dump_agent);

const DUMP_TASK_SIZE: u32 = size_of::<humpty::DumpTask>() as u32;
const HEADER_SIZE: u32 = size_of::<humpty::DumpAreaHeader>() as u32;
const SEGMENT_DATA_SIZE: u32 = size_of::<humpty::DumpSegmentData>() as u32;

#[derive(Copy, Clone)]
struct ClientDumpState {
    /// Key used to distinguish between clients (should be random)
    key: [u8; 16],

    /// Expected sequence number
    expected_seq: u32,

    /// Current position, associated with `expected_seq`
    current_pos: Position,

    /// Previous position, associated with `expected_seq - 1`
    prev_pos: Option<Position>,
}

#[derive(Copy, Clone)]
struct Position {
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
            let data = self
                .agent
                .read_dump(index, 0)
                .map_err(|_e| SpError::Dump(DumpError::BadArea))?;
            let header = humpty::DumpAreaHeader::read_from(
                &data[..HEADER_SIZE as usize],
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
        key: [u8; 16],
    ) -> Result<DumpTask, SpError> {
        // Find the area where this dump starts
        let mut count = 0;
        let mut found = None;
        let mut data = [0u8; 256];
        let mut header = humpty::DumpAreaHeader::new_zeroed();
        for index in 0.. {
            data = self
                .agent
                .read_dump(index, 0)
                .map_err(|_e| SpError::Dump(DumpError::BadArea))?;
            header = humpty::DumpAreaHeader::read_from(
                &data[..HEADER_SIZE as usize],
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
        let offset =
            HEADER_SIZE + u32::from(header.nsegments) * SEGMENT_DATA_SIZE;
        let task = humpty::DumpTask::read_from_prefix(&data[offset as usize..])
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
            expected_seq: 0,
            current_pos: Position {
                area_index: index,
                offset: offset + DUMP_TASK_SIZE,
            },
            prev_pos: None,
        });

        Ok(DumpTask {
            task: task.id,
            time: task.time,
            compression: DumpCompression::Lzss,
        })
    }

    fn clear_client_state(&mut self, key: [u8; 16]) {
        self.clients
            .iter_mut()
            .filter(|c| c.map(|c| c.key == key).unwrap_or(false))
            .for_each(|c| *c = None)
    }

    pub(crate) fn task_dump_read_continue(
        &mut self,
        key: [u8; 16],
        seq: u32,
        buf: &mut [u8],
    ) -> Result<Option<DumpSegment>, SpError> {
        let r = self.task_dump_read_continue_inner(key, seq, buf);
        if matches!(r, Ok(None) | Err(..)) {
            self.clear_client_state(key);
        }
        r
    }

    pub(crate) fn task_dump_read_continue_inner(
        &mut self,
        key: [u8; 16],
        seq: u32,
        buf: &mut [u8],
    ) -> Result<Option<DumpSegment>, SpError> {
        let Some(state) =
            self.clients.iter_mut().flatten().find(|c| c.key == key)
        else {
            return Err(SpError::Dump(DumpError::BadKey));
        };

        // Figure out if we're replaying a previous chunk or not
        let replay = if seq == state.expected_seq {
            false
        } else if state.expected_seq > 0 && seq == state.expected_seq - 1 {
            true
        } else {
            return Err(SpError::Dump(DumpError::BadSequenceNumber));
        };

        // We'll update this position as we go, then write it back afterwards
        let mut pos = if replay {
            state.prev_pos.unwrap_lite()
        } else {
            state.current_pos
        };

        let mut header = humpty::DumpAreaHeader::new_zeroed();
        self.agent
            .read_dump_into(pos.area_index, 0, header.as_bytes_mut())
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;

        // Make sure the header is still valid
        if header.contents != humpty::DumpContents::SingleTask.into() {
            return Err(SpError::Dump(DumpError::NoLongerValid));
        }

        // Move along to the next area if we're at the end
        if pos.offset + SEGMENT_DATA_SIZE > header.written {
            if header.next == 0 {
                // we're done, because there's no more dump areas
                return Ok(None);
            }

            // Move to the next area and read the header
            pos.area_index += 1;
            self.agent
                .read_dump_into(pos.area_index, 0, header.as_bytes_mut())
                .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;

            // If the next header is of a different type, then we have no more
            // data left to read and can return None.
            if header.contents != humpty::DumpContents::SingleTask.into()
                || header.nsegments != 0
            {
                return Ok(None);
            }
            // Skip the area header
            pos.offset = HEADER_SIZE;
        }

        // Read the dump segment data header
        let mut ds = humpty::DumpSegmentData::new_zeroed();
        self.agent
            .read_dump_into(pos.area_index, pos.offset, ds.as_bytes_mut())
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;
        pos.offset += SEGMENT_DATA_SIZE;

        // Read the compressed bytes directly into the tx buffer
        if ds.compressed_length as usize > buf.len() {
            return Err(SpError::Dump(DumpError::SegmentTooLong));
        }
        self.agent
            .read_dump_into(
                pos.area_index,
                pos.offset,
                &mut buf[..ds.compressed_length as usize],
            )
            .map_err(|_e| SpError::Dump(DumpError::ReadFailed))?;
        pos.offset += ds.compressed_length as u32;
        while pos.offset as usize & humpty::DUMP_SEGMENT_MASK != 0 {
            pos.offset += 1;
        }

        if !replay {
            state.prev_pos = Some(state.current_pos);
            state.current_pos = pos;
            state.expected_seq += 1;
        }

        Ok(Some(DumpSegment {
            address: ds.address,
            compressed_length: ds.compressed_length,
            uncompressed_length: ds.uncompressed_length,
            seq,
        }))
    }
}
