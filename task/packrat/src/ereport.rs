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
use minicbor_lease::LeasedWriter;
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
    PartNumberNotUtf8,
    SerialNumberNotUtf8,
}

#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum EreportError {
    TaskIdOutOfRange,
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
        mut data: Leased<idol_runtime::W, [u8]>,
        vpd: Option<&VpdIdentity>,
    ) -> Result<usize, RequestError<EreportReadError>> {
        ringbuf_entry!(Trace::ReadRequest {
            restart_id: restart_id.into()
        });

        let current_restart_id =
            self.restart_id.ok_or(EreportReadError::RestartIdNotSet)?;
        // Skip over a header-sized initial chunk.
        let first_data_byte = size_of::<ereport_messages::ResponseHeader>();
        let mut first_written_ena = None;

        let mut encoder = minicbor::Encoder::new(LeasedWriter::starting_at(
            first_data_byte,
            &mut data,
        ));

        fn check_err(
            encoder: &minicbor::Encoder<LeasedWriter<'_, idol_runtime::W>>,
            err: minicbor::encode::Error<minicbor_lease::WriteError>,
        ) -> RequestError<EreportReadError> {
            match encoder.writer().check_err(err) {
                minicbor_lease::Error::WentAway => ClientError::WentAway.fail(),
                minicbor_lease::Error::EndOfLease => {
                    ClientError::BadLease.fail()
                }
            }
        }

        // Start the metadata map.
        //
        // MGS expects us to always include this, and to just have it be
        // empty if we didn't send any metadata.
        encoder
            .begin_map()
            // This pattern (which will occur every time we handle an encoder
            // error in this function) is goofy, but is necessary to placate the
            // borrow checker: the function passed to `map_err` must borrow the
            // encoder so that it can check whether the error indicates that we
            // ran out of space in the lease, or if the client disappeared.
            // However, because `minicbor::Encoder`'s methods return a mutable
            // borrow of the encoder, we must drop it first before borrowing it
            // into the `map_err` closure. Thus, every `map_err` must be
            // preceded by a `map` with a toilet closure that drops the mutable
            // borrow. Yuck.
            .map(|_| ())
            .map_err(|e| check_err(&encoder, e))?;

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
                // Encode the metadata map.
                self.encode_metadata(&mut encoder, vpd)
                    .map(|_| ())
                    .map_err(|e| check_err(&encoder, e))?;
                ringbuf_entry!(Trace::MetadataEncoded {
                    len: encoder
                        .writer()
                        .position()
                        .saturating_sub(first_data_byte)
                        as u32,
                });
            }
            // Begin at ENA 0
            0
        };

        // End metadata map.
        encoder
            .end()
            .map(|_| ())
            .map_err(|e| check_err(&encoder, e))?;

        let mut reports = 0;
        // Beginning with the first
        for r in self.storage.read_from(begin_ena) {
            if reports >= limit {
                break;
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

            // If there's no room left for this one in the lease, we're
            // done here. Note that the use of `>=` rather than `>` is
            // intentional, as we want to ensure that there's room for
            // the final `CBOR_BREAK` byte that ends the CBOR array.
            //
            // N.B.: saturating_add is used to avoid panic sites --- if this
            // adds up to `u32::MAX` it *definitely* won't fit :)
            let len_needed = encoder
                .writer()
                .position()
                // Of course, we need enough space for the ereport itself...
                .saturating_add(minicbor::len(&entry))
                // In addition, if we haven't yet started the CBOR array of
                // ereports, we need a byte for that too.
                .saturating_add(first_written_ena.is_none() as usize);
            if len_needed >= encoder.writer().lease().len() {
                break;
            }

            if first_written_ena.is_none() {
                first_written_ena = Some(r.ena);
                // Start the ereport array
                encoder
                    .begin_array()
                    .map(|_| ())
                    .map_err(|e| check_err(&encoder, e))?;
            }
            encoder
                .encode(&entry)
                .map(|_| ())
                .map_err(|e| check_err(&encoder, e))?;
            reports += 1;
        }

        if let Some(start_ena) = first_written_ena {
            // End CBOR array, if we wrote anything.
            encoder
                .end()
                .map(|_| ())
                .map_err(|e| check_err(&encoder, e))?;

            ringbuf_entry!(Trace::Reported {
                start_ena,
                reports,
                limit
            });
        }

        // Release the mutable borrow on the lease so we can write the header.
        let end = encoder.into_writer().position();
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
        Ok(end)
    }

    fn encode_metadata(
        &self,
        encoder: &mut minicbor::Encoder<LeasedWriter<'_, idol_runtime::W>>,
        vpd: &VpdIdentity,
    ) -> Result<(), minicbor::encode::Error<minicbor_lease::WriteError>> {
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
        encoder.str("baseboard_rev")?.u32(vpd.revision)?;
        Ok(())
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

mod config {
    include!(concat!(env!("OUT_DIR"), "/ereport_config.rs"));
}
