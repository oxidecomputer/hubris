// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Packrat ereport aggregation.
//!
//! As described in [RFD 545 § 4.3], `packrat`'s role in the ereport subsystem
//! is to aggregate ereports from other tasks in a circular buffer. Ereports are
//! submitted to `packrat` via the `deliver_ereport` IPC call. The `snitch` task
//! requests ereports from `packrat` using the `read_ereports` IPC call, which
//! also flushes committed ereports from the buffer.
//!
//! [RFD 545 § 4.3]: https://rfd.shared.oxide.computer/rfd/0545#_aggregation

use super::ereport_messages;

use core::convert::Infallible;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use minicbor::CborLen;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_packrat_api::VpdIdentity;
use userlib::{sys_get_timer, RecvMessage, TaskId, UnwrapLite};
use zerocopy::IntoBytes;

pub(crate) struct EreportStore {
    storage: &'static mut snitch_core::Store<STORE_SIZE>,
    recv: &'static mut [u8; RECV_BUF_SIZE],
    next_ena: u64,
    restart_id: ereport_messages::RestartId,
}

pub(crate) struct EreportBufs {
    storage: snitch_core::Store<STORE_SIZE>,
    recv: [u8; RECV_BUF_SIZE],
}

/// Number of bytes of RAM dedicated to ereport storage. Each individual
/// report consumes a small amount of this (currently 12 bytes).
const STORE_SIZE: usize = 4096;

/// Number of bytes for the receive buffer. This only needs to fit a single
/// ereport at a time (and implicitly, limits the maximum size of an ereport).
pub(crate) const RECV_BUF_SIZE: usize = 1024;

userlib::task_slot!(RNG, rng_driver);

/// Separate ring buffer for ereport events, as we probably don't care that much
/// about the sequence of ereport events relative to other packrat API events.
#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum EreportTrace {
    #[count(skip)]
    None,
    EreportDelivered {
        src: TaskId,
        len: u32,
    },
    Flushed {
        ena: u64,
    },
    RestartIdMismatch {
        current: u128,
        requested: u128,
    },
    Reported {
        start_ena: u64,
        reports: u8,
        limit: u8,
    },
}
counted_ringbuf!(EreportTrace, 16, EreportTrace::None);

impl EreportStore {
    pub(crate) fn new(
        EreportBufs {
            ref mut storage,
            ref mut recv,
        }: &'static mut EreportBufs,
    ) -> Self {
        let rng = drv_rng_api::Rng::from(RNG.get_task_id());
        let mut buf = [0u8; 16];
        // XXX(eliza): if this fails we are TURBO SCREWED...
        rng.fill(&mut buf).unwrap_lite();
        let restart_id =
            ereport_messages::RestartId::from(u128::from_le_bytes(buf));

        storage.initialize(0, 0); // TODO tid timestamp

        Self {
            storage,
            recv,
            next_ena: 0,
            restart_id,
        }
    }
}

impl EreportStore {
    pub(crate) fn deliver_ereport(
        &mut self,
        msg: &RecvMessage,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, RECV_BUF_SIZE>,
    ) -> Result<(), RequestError<Infallible>> {
        data.read_range(0..data.len(), self.recv)
            .map_err(|_| ClientError::WentAway.fail())?;
        let timestamp = sys_get_timer().now;
        self.storage
            .insert(msg.sender.0, timestamp, &self.recv[..data.len()]);
        // TODO(eliza): would maybe be nice to say something if the ereport got
        // eaten...
        ringbuf_entry!(EreportTrace::EreportDelivered {
            src: msg.sender,
            len: data.len() as u32
        });
        Ok(())
    }

    pub(crate) fn read_ereports(
        &mut self,
        request_id: ereport_messages::RequestIdV0,
        restart_id: ereport_messages::RestartId,
        begin_ena: ereport_messages::Ena,
        limit: u8,
        committed_ena: ereport_messages::Ena,
        data: Leased<idol_runtime::W, [u8]>,
        vpd: Option<&VpdIdentity>,
    ) -> Result<usize, RequestError<Infallible>> {
        // Skip over a header-sized initial chunk.
        let first_data_byte = size_of::<ereport_messages::ResponseHeader>();

        let mut position = first_data_byte;
        let mut first_written_ena = None;

        // Begin metadata map.
        data.write_at(position, 0xbf)
            .map_err(|_| ClientError::WentAway.fail())?;
        position += 1;

        // If the requested restart ID matches the current restart ID, then read
        // from the requested ENA. If not, start at ENA 0.
        let begin_ena = if restart_id == self.restart_id {
            // If the restart ID matches, flush previous ereports up to
            // `committed_ena`, if there is one.
            if committed_ena != ereport_messages::Ena::NONE {
                self.storage.flush_thru(committed_ena.into());
                ringbuf_entry!(EreportTrace::Flushed {
                    ena: committed_ena.into()
                });
            }
            begin_ena.into()
        } else {
            ringbuf_entry!(EreportTrace::RestartIdMismatch {
                requested: restart_id.into(),
                current: self.restart_id.into()
            });

            // Encode the metadata map into our buffer.
            // TODO(eliza): this will panic if the encoded metadata map is
            // longer than 1024B...currently it should never be that, given that
            // everything we encode here is fixed-size. But, yuck...
            let c = minicbor::encode::write::Cursor::new(&mut self.recv[..]);
            let mut encoder = minicbor::Encoder::new(c);
            if let Some(vpd) = vpd {
                encoder
                    .str("baseboard_part_number")
                    .unwrap_lite()
                    .bytes(vpd.part_number.as_bytes())
                    .unwrap_lite()
                    .str("baseboard_serial_number")
                    .unwrap_lite()
                    .bytes(vpd.serial.as_bytes())
                    .unwrap_lite()
                    .str("rev")
                    .unwrap_lite()
                    .u32(vpd.revision)
                    .unwrap_lite();
            }

            // Write the encoded metadata map.
            let size = encoder.into_writer().position();
            data.write_range(position..position + size, &self.recv[..size])
                .map_err(|_| ClientError::WentAway.fail())?;
            position += size;

            // Begin at ENA 0
            0
        };

        // End metadata map.
        data.write_at(position, 0xff)
            .map_err(|_| ClientError::WentAway.fail())?;
        position += 1;

        let mut reports = 0;
        // Beginning with the first
        for r in self.storage.read_from(begin_ena) {
            if reports >= limit {
                break;
            }

            if first_written_ena.is_none() {
                first_written_ena = Some(r.ena);
                // Start CBOR list
                // XXX(eliza): in theory it might be nicer to use
                // `minicbor::data::Token::BeginArray` here, but it's way more
                // annoying in practice...
                data.write_at(position, 0x9f)
                    .map_err(|_| ClientError::WentAway.fail())?;
                position += 1;
            }

            let tid = TaskId(r.tid);
            let task_name = hubris_task_names::TASK_NAMES
                .get(tid.index())
                .copied()
                .unwrap_or({
                    // This represents an internal error, where we've recorded
                    // an out-of-range task ID somehow. We still want to get the
                    // ereport out, so we'll use a recognizable but illegal task
                    // name to indicate that it's missing.
                    "-" // TODO
                });
            let generation = tid.generation();

            let entry = (
                task_name,
                u8::from(generation),
                r.timestamp,
                ByteGather(r.slices.0, r.slices.1),
            );
            let mut c =
                minicbor::encode::write::Cursor::new(&mut self.recv[..]);
            match minicbor::encode(&entry, &mut c) {
                Ok(()) => {
                    let size = c.position();
                    // If there's no room left for this one in the lease, we're
                    // done here.
                    if position + size >= data.len() {
                        break;
                    }
                    data.write_range(
                        position..position + size,
                        &self.recv[..size],
                    )
                    .map_err(|_| ClientError::WentAway.fail())?;
                    position += size;
                    reports += 1;
                }
                Err(_end) => {
                    // This is an odd one; we've admitted a record into our
                    // queue that won't fit in our buffer. This can happen
                    // because of the encoding overhead, in theory, but should
                    // be prevented.
                    // TODO
                }
            }
        }

        if let Some(start_ena) = first_written_ena {
            // End CBOR list, if we wrote anything.
            data.write_at(position, 0xff)
                .map_err(|_| ClientError::WentAway.fail())?;
            position += 1;

            ringbuf_entry!(EreportTrace::Reported {
                start_ena,
                reports,
                limit
            });
        }

        let first_ena = first_written_ena.unwrap_or(self.next_ena);
        let header = ereport_messages::ResponseHeader::V0(
            ereport_messages::ResponseHeaderV0 {
                request_id,
                restart_id: self.restart_id,
                start_ena: first_ena.into(),
            },
        );
        data.write_range(0..size_of_val(&header), header.as_bytes())
            .map_err(|_| ClientError::WentAway.fail())?;
        Ok(position)
    }
}

impl EreportBufs {
    pub(crate) const fn new() -> Self {
        Self {
            storage: snitch_core::Store::DEFAULT,
            recv: [0u8; RECV_BUF_SIZE],
        }
    }
}

struct ByteGather<'a, 'b>(&'a [u8], &'b [u8]);

impl<C> minicbor::Encode<C> for ByteGather<'_, '_> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes_len((self.0.len() + self.1.len()) as u64)?;
        e.writer_mut()
            .write_all(self.0)
            .map_err(minicbor::encode::Error::write)?;
        e.writer_mut()
            .write_all(self.1)
            .map_err(minicbor::encode::Error::write)?;
        Ok(())
    }
}

impl<C> CborLen<C> for ByteGather<'_, '_> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        let n = self.0.len() + self.1.len();
        n.cbor_len(ctx) + n
    }
}
