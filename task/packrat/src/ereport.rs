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
use task_packrat_api::{EreportReadError, VpdIdentity};
use userlib::{kipc, sys_get_timer, RecvMessage, TaskId};
use zerocopy::IntoBytes;

pub(crate) struct EreportStore {
    storage: &'static mut snitch_core::Store<STORE_SIZE>,
    recv: &'static mut [u8; RECV_BUF_SIZE],
    image_id: [u8; 8],
    pub(super) restart_id: Option<ereport_messages::RestartId>,
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

/// Separate ring buffer for ereport events, as we probably don't care that much
/// about the sequence of ereport events relative to other packrat API events.
#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    EreportReceived {
        src: TaskId,
        len: u32,
        #[count(children)]
        result: snitch_core::InsertResult,
    },
    ReadRequest {
        restart_id: u128,
    },
    Flushed {
        committed_ena: u64,
        flushed: usize,
    },
    RestartIdMismatch {
        current_restart_id: u128,
    },
    MetadataError(#[count(children)] MetadataError),
    MetadataEncoded {
        len: u32,
    },
    EreportError(#[count(children)] EreportError),
    Reported {
        start_ena: u64,
        reports: u8,
        limit: u8,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum MetadataError {
    TooLong,
    PartNumberNotUtf8,
    SerialNumberNotUtf8,
}

#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum EreportError {
    TaskIdOutOfRange,
    TooLong,
}

counted_ringbuf!(Trace, 16, Trace::None);

impl EreportStore {
    pub(crate) fn new(
        EreportBufs {
            ref mut storage,
            ref mut recv,
        }: &'static mut EreportBufs,
    ) -> Self {
        let now = sys_get_timer().now;
        storage.initialize(config::TASK_ID, now);
        let image_id = {
            let id = kipc::read_image_id();
            u64::to_le_bytes(id)
        };

        Self {
            storage,
            recv,
            image_id,
            restart_id: None,
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
        let result = self.storage.insert(
            msg.sender.0,
            timestamp,
            &self.recv[..data.len()],
        );
        ringbuf_entry!(Trace::EreportReceived {
            src: msg.sender,
            len: data.len() as u32,
            result,
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
    ) -> Result<usize, RequestError<EreportReadError>> {
        ringbuf_entry!(Trace::ReadRequest {
            restart_id: restart_id.into()
        });

        /// Byte indicating the end of an indeterminate-length CBOR array or
        /// map.
        const CBOR_BREAK: u8 = 0xff;

        let current_restart_id =
            self.restart_id.ok_or(EreportReadError::RestartIdNotSet)?;
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
        let begin_ena = if restart_id == current_restart_id {
            // If the restart ID matches, flush previous ereports up to
            // `committed_ena`, if there is one.
            if committed_ena != ereport_messages::Ena::NONE {
                let flushed = self.storage.flush_thru(committed_ena.into());
                ringbuf_entry!(Trace::Flushed {
                    committed_ena: committed_ena.into(),
                    flushed,
                });
            }
            begin_ena.into()
        } else {
            ringbuf_entry!(Trace::RestartIdMismatch {
                current_restart_id: current_restart_id.into()
            });

            // If we don't have our VPD identity yet, don't send any metadata.
            //
            // We *could* include the Hubris image ID here even if our VPD
            // identity hasn't been set, but sending an empty metadata map
            // ensures that MGS will continue asking for metadata on subsequent
            // requests.
            if let Some(vpd) = vpd {
                // Encode the metadata map into our buffer.
                match self.encode_metadata(vpd) {
                    Ok(encoded) => {
                        data.write_range(
                            position..position + encoded.len(),
                            encoded,
                        )
                        .map_err(|_| ClientError::WentAway.fail())?;

                        position += encoded.len();

                        ringbuf_entry!(Trace::MetadataEncoded {
                            len: encoded.len() as u32
                        });
                    }
                    Err(err) => {
                        // Encoded VPD metadata was too long, or couldn't be
                        // represented as a CBOR string.
                        ringbuf_entry!(Trace::MetadataError(err));
                    }
                }
            }
            // Begin at ENA 0
            0
        };

        // End metadata map.
        data.write_at(position, CBOR_BREAK)
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
                .unwrap_or_else(|| {
                    // This represents an internal error, where we've recorded
                    // an out-of-range task ID somehow. We still want to get the
                    // ereport out, so we'll use a recognizable but illegal task
                    // name to indicate that it's missing.
                    ringbuf_entry!(Trace::EreportError(
                        EreportError::TaskIdOutOfRange
                    ));
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
                    // done here. Note that the use of `>=` rather than `>` is
                    // intentional, as we want to ensure that there's room for
                    // the final `CBOR_BREAK` byte that ends the CBOR array.
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
                    ringbuf_entry!(Trace::EreportError(EreportError::TooLong));
                }
            }
        }

        if let Some(start_ena) = first_written_ena {
            // End CBOR list, if we wrote anything.
            data.write_at(position, CBOR_BREAK)
                .map_err(|_| ClientError::WentAway.fail())?;
            position += 1;

            ringbuf_entry!(Trace::Reported {
                start_ena,
                reports,
                limit
            });
        }

        let first_ena = first_written_ena.unwrap_or(0);
        let header = ereport_messages::ResponseHeader::V0(
            ereport_messages::ResponseHeaderV0 {
                request_id,
                restart_id: current_restart_id,
                start_ena: first_ena.into(),
            },
        );
        data.write_range(0..size_of_val(&header), header.as_bytes())
            .map_err(|_| ClientError::WentAway.fail())?;
        Ok(position)
    }

    fn encode_metadata(
        &mut self,
        vpd: &VpdIdentity,
    ) -> Result<&[u8], MetadataError> {
        let c = minicbor::encode::write::Cursor::new(&mut self.recv[..]);
        let mut encoder = minicbor::Encoder::new(c);
        // TODO(eliza): presently, this code bails out if the metadata map gets
        // longer than our buffer. It would be nice to have a way to keep the
        // encoded metadata up to the last complete key-value pair...
        encoder
            .str("hubris_archive_id")?
            .bytes(&self.image_id[..])?;
        match core::str::from_utf8(&vpd.part_number[..]) {
            Ok(part_number) => {
                encoder.str("baseboard_part_number")?.str(part_number)?;
            }
            Err(_) => ringbuf_entry!(Trace::MetadataError(
                MetadataError::PartNumberNotUtf8
            )),
        }
        match core::str::from_utf8(&vpd.serial[..]) {
            Ok(serial_number) => {
                encoder.str("baseboard_serial_number")?.str(serial_number)?;
            }
            Err(_) => ringbuf_entry!(Trace::MetadataError(
                MetadataError::SerialNumberNotUtf8
            )),
        }
        encoder.str("rev")?.u32(vpd.revision)?;
        let size = encoder.into_writer().position();
        Ok(&self.recv[..size])
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
        e.bytes_len((self.0.len().wrapping_add(self.1.len())) as u64)?;
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
        let n = self.0.len().wrapping_add(self.1.len());
        n.cbor_len(ctx).wrapping_add(n)
    }
}

impl From<minicbor::encode::write::EndOfSlice> for MetadataError {
    fn from(_: minicbor::encode::write::EndOfSlice) -> MetadataError {
        MetadataError::TooLong
    }
}

mod config {
    include!(concat!(env!("OUT_DIR"), "/ereport_config.rs"));
}
