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

use drv_caboose::CabooseReader;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use minicbor::{encode, CborLen};
use minicbor_lease::LeasedWriter;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_packrat_api::{EreportReadError, EreportWriteError, OxideIdentity};
use userlib::{
    kipc, sys_get_timer, FaultInfo, FaultSource, Generation,
    ReadPanicMessageError, RecvMessage, ReplyFaultReason, TaskId, TaskState,
    UsageError,
};
use zerocopy::IntoBytes;

pub(crate) struct EreportStore {
    storage: &'static mut snitch_core::Store<STORE_SIZE>,
    recv: &'static mut [u8; RECV_BUF_SIZE],
    panic_buf: &'static mut [u8; userlib::PANIC_MESSAGE_MAX_LEN],
    task_gens: &'static mut [TaskHistory; hubris_num_tasks::NUM_TASKS],
    pub(super) restart_id: Option<ereport_messages::RestartId>,
    holding_faults: bool,
}

pub(crate) struct EreportBufs {
    storage: snitch_core::Store<STORE_SIZE>,
    recv: [u8; RECV_BUF_SIZE],
    panic_buf: [u8; userlib::PANIC_MESSAGE_MAX_LEN],
    task_gens: [TaskHistory; hubris_num_tasks::NUM_TASKS],
}

#[derive(Copy, Clone)]
struct TaskHistory {
    last_gen: u8,
    tfaulted: Option<u64>,
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
    MetadataSkippedNoVpd,
    BadCabooseValue {
        tag: [u8; 4],
        #[count(children)]
        err: drv_caboose::CabooseError,
    },
    CabooseValueNotUtf8 {
        tag: [u8; 4],
    },
    MetadataEncoded {
        len: u32,
    },
    EreportError(#[count(children)] EreportError),
    Reported {
        start_ena: u64,
        reports: u8,
        limit: u8,
    },
    HoldingFaults(bool),
    FaultRecorded {
        task: TaskId,
        #[count(children)]
        result: snitch_core::InsertResult,
        len: usize,
    },
    TaskFaulted {
        task: TaskId,
        nfaults: usize,
    },
    // A fault report was >1024B long! what the heck!
    GiantFaultReport {
        task: TaskId,
    },
    MissedPanicMessage {
        task: TaskId,
    },
    BadPanicMessage {
        task: TaskId,
    },
    TaskAlreadyRecovered {
        task: TaskId,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum MetadataError {
    PartNumberNotUtf8,
    SerialNumberNotUtf8,
    NoCaboose,
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
            ref mut panic_buf,
            ref mut task_gens,
        }: &'static mut EreportBufs,
    ) -> Self {
        let now = sys_get_timer().now;
        storage.initialize(config::TASK_ID, now);

        Self {
            storage,
            recv,
            panic_buf,
            task_gens,
            restart_id: None,
            holding_faults: false,
        }
    }
}

impl EreportStore {
    pub(crate) fn deliver_ereport(
        &mut self,
        msg: &RecvMessage,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, RECV_BUF_SIZE>,
    ) -> Result<(), RequestError<EreportWriteError>> {
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
        match result {
            snitch_core::InsertResult::Inserted => Ok(()),
            snitch_core::InsertResult::Lost => {
                Err(RequestError::from(EreportWriteError::Lost))
            }
        }
    }

    pub(crate) fn read_ereports(
        &mut self,
        request_id: ereport_messages::RequestIdV0,
        restart_id: ereport_messages::RestartId,
        begin_ena: ereport_messages::Ena,
        limit: u8,
        committed_ena: ereport_messages::Ena,
        mut data: Leased<idol_runtime::W, [u8]>,
        vpd: Option<&OxideIdentity>,
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

        fn handle_encode_err(
            err: minicbor::encode::Error<minicbor_lease::Error>,
        ) -> RequestError<EreportReadError> {
            // These should always be write errors; everything we write should
            // always encode successfully.
            match err.into_write() {
                Some(e) => ClientError::from(e).fail(),
                // This really shouldn't ever happen: an error that didn't come
                // from the underlying lease writer means that we couldn't
                // encode the ereport as CBOR. Since the structure of these list
                // entries is always the same, they really had better always be
                // well-formed CBOR. But, since Packrat is never supposed to
                // panic, let's just kill the client instead of us.
                None => ClientError::BadMessageContents.fail(),
            }
        }

        // Start the metadata map.
        //
        // MGS expects us to always include this, and to just have it be
        // empty if we didn't send any metadata.
        encoder.begin_map().map_err(handle_encode_err)?;

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

                // If we have freed up space in the buffer, and we are aware of
                // task faults which previously did not fit in the buffer, try
                // to say something about them.
                if flushed > 0 && self.holding_faults {
                    ringbuf_entry!(Trace::HoldingFaults(true));
                    self.record_faulted_tasks(userlib::sys_get_timer().now);
                }
            }
            begin_ena.into()
        } else {
            ringbuf_entry!(Trace::RestartIdMismatch {
                current_restart_id: current_restart_id.into()
            });

            // If we don't have our VPD identity yet, don't send any metadata.
            //
            // We *could* include the caboose metadata here even if our VPD
            // identity hasn't been set, but sending an empty metadata map
            // ensures that MGS will continue asking for metadata on subsequent
            // requests.
            if let Some(vpd) = vpd {
                // Encode the metadata map.
                self.encode_metadata(&mut encoder, vpd)
                    .map_err(handle_encode_err)?;
                ringbuf_entry!(Trace::MetadataEncoded {
                    len: encoder
                        .writer()
                        .position()
                        .saturating_sub(first_data_byte)
                        as u32,
                });
            } else {
                ringbuf_entry!(Trace::MetadataSkippedNoVpd);
            }
            // Begin at ENA 0
            0
        };

        // End metadata map.
        encoder.end().map_err(handle_encode_err)?;

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
                encoder.begin_array().map_err(handle_encode_err)?;
            }
            encoder.encode(&entry).map_err(handle_encode_err)?;
            reports += 1;
        }

        if let Some(start_ena) = first_written_ena {
            // End CBOR array, if we wrote anything.
            encoder.end().map_err(handle_encode_err)?;

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
        vpd: &OxideIdentity,
    ) -> Result<(), encode::Error<minicbor_lease::Error>> {
        /// Attempt to grab a value from the caboose and stuff it into the CBOR
        /// encoder.
        fn caboose_value(
            tag: [u8; 4],
            reader: &mut CabooseReader<'_>,
            encoder: &mut minicbor::Encoder<LeasedWriter<'_, idol_runtime::W>>,
        ) -> Result<(), encode::Error<minicbor_lease::Error>> {
            let value = match reader.get(tag) {
                Ok(value) => value,
                Err(err) => {
                    ringbuf_entry!(Trace::BadCabooseValue { tag, err });
                    encoder.null()?;
                    return Ok(());
                }
            };
            match core::str::from_utf8(value) {
                Ok(value) => encoder.str(value)?,
                Err(_) => {
                    ringbuf_entry!(Trace::CabooseValueNotUtf8 { tag });
                    encoder.null()?
                }
            };
            Ok(())
        }

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

        encoder.str("hubris_caboose")?;
        match drv_caboose_pos::CABOOSE_POS.as_slice() {
            Some(caboose) => {
                let mut caboose = CabooseReader::new(caboose);
                // "caboose": {
                //     "board": "gimlet-c",
                //     "version": "...",
                //     "commit": "..."
                // }
                encoder.begin_map()?;
                encoder.str("board")?;
                caboose_value(*b"BORD", &mut caboose, encoder)?;
                encoder.str("version")?;
                caboose_value(*b"VERS", &mut caboose, encoder)?;
                encoder.str("commit")?;
                caboose_value(*b"GITC", &mut caboose, encoder)?;
                encoder.end()?;
            }
            None => {
                ringbuf_entry!(Trace::MetadataError(MetadataError::NoCaboose));
                encoder.null()?;
            }
        }
        Ok(())
    }

    pub(crate) fn record_faulted_tasks(&mut self, now: u64) {
        use core::cmp::Ordering;

        let mut nfaulted: usize = 0;
        let mut nreported: usize = 0;
        for (task_index, state) in self.task_gens.iter_mut().enumerate() {
            let TaskHistory {
                ref mut last_gen,
                ref mut tfaulted,
            } = state;

            let task = TaskId::for_index_and_gen(
                task_index as usize,
                Generation::from(*last_gen),
            );
            let task = userlib::sys_refresh_task_id(task);

            // Check if the generation number has changed to determine whether
            // the task has faulted.
            let curr_gen = u8::from(task.generation());
            let (nfaults, timestamp) = match (curr_gen.cmp(last_gen), *tfaulted)
            {
                // If the current generation is less than the last generation we
                // saw, then the counter has wrapped around. The number of times
                // the task has faulted is the difference between the prior
                // generation and `u8::MAX`, plus the new generation.
                //
                // This is a bit fudgey if the generation counter has wrapped
                // multiple times since the last we saw it, but there's no good
                // way to detect that and that would mean a *lot* of faults in a
                // short time...
                (Ordering::Less, _) => {
                    let nfaults = (u8::MAX as usize - *last_gen as usize)
                        + curr_gen as usize;
                    (nfaults, now)
                }
                // Task has not faulted, so just move on to the next one.
                (Ordering::Equal, None) => continue,
                // No *new* faults have occurred, but we are holding a
                // previously observed fault for this task that was not
                // successfully reported.
                (Ordering::Equal, Some(tfaulted)) => (1, tfaulted),
                // The generation has increased, so the number of faults we haven't
                // seen is just the difference between the new and current
                // counts.
                (Ordering::Greater, _) => {
                    let nfaults = curr_gen as usize - *last_gen as usize;
                    (nfaults, now)
                }
            };

            // This will never wrap, since there can't be more than
            // `hubris_num_tasks::NUM_TASKS` tasks that have faulted, but the
            // compiler doesn't know this.
            nfaulted = nfaulted.wrapping_add(1);
            ringbuf_entry!(Trace::TaskFaulted { task, nfaults });

            if let Ok(ereport) = Self::record_faulted_task(
                &mut self.recv[..],
                &mut self.panic_buf,
                task,
                nfaults,
            ) {
                let result = self.storage.insert(task.0, timestamp, ereport);
                ringbuf_entry!(Trace::FaultRecorded {
                    task,
                    result,
                    len: ereport.len()
                });
                match result {
                    snitch_core::InsertResult::Inserted => {
                        // We successfully made an ereport for this fault! Update
                        // our tracked generation for this task.
                        *last_gen = curr_gen;
                        // Again, won't ever actually wrap, but whatever.
                        nreported = nreported.wrapping_add(1);
                        *tfaulted = None;
                    }
                    snitch_core::InsertResult::Lost => {
                        // No ereport was recorded, so *don't* acknowledge the fault
                        // by updating our tracked generation. This way we will
                        // still treat the task as having faulted in the past and
                        // will attempt to make an ereport for it later, if there's
                        // space.
                        *tfaulted = Some(timestamp);
                    }
                }
            } else {
                // The fault ereport was >1024B long and thus will never fit
                // in a UDP frame.
                //
                // This should basically be impossible, since there's an
                // upper bound on the size of the fault report, but we
                // should handle it gracefully rather than panicking
                // Packrat, if I'm wrong.
                ringbuf_entry!(Trace::GiantFaultReport { task });
                // Treat the fault as acked, because if it was >1024B this
                // time, it will always be >1024B next time.
                *last_gen = curr_gen;
                // Again, won't ever actually wrap, but whatever.
                nreported = nreported.wrapping_add(1);
            };
        }

        // If we successfully recorded an ereport for every task we observed to
        // have faulted, we are no longer "holding" unreported faults.
        self.holding_faults = !(nreported == nfaulted);
        ringbuf_entry!(Trace::HoldingFaults(self.holding_faults));
    }

    /// Record an ereport indicating that a Hubris task has faulted.
    ///
    /// Ereports for hardware faults are largely intended to be interpreted by
    /// the automated fault-management system. The Hubris task ereports we
    /// generate here, on the other hand, generally represent a firmware bug
    /// rather than an anticipated hardware failure, and therefore, we expect
    /// that it is much likelier that the ereport will be read by a human being.
    /// Thus, we err on the side of human-readability somewhat with their
    /// contents.
    fn record_faulted_task<'buf>(
        buf: &'buf mut [u8],
        panic_buf: &mut [u8; userlib::PANIC_MESSAGE_MAX_LEN],
        task: TaskId,
        nfaults: usize,
    ) -> Result<&'buf [u8], encode::Error<encode::write::EndOfSlice>> {
        /// Encode a CBOR object representing another task that was involved in a
        /// fault; either the injecting task in a `FaultInfo::Injected`, or the
        /// server that responded with a `REPLY_FAULT` in a
        /// `FaultInfo::FromServer`.
        ///
        /// The encoded CBOR looks like this:
        /// ```json
        /// {
        ///     "task": "task_name",
        ///     "gen": 1
        /// }
        /// ```
        fn encode_task<W: encode::Write>(
            encoder: &mut minicbor::Encoder<W>,
            task: TaskId,
        ) -> Result<(), encode::Error<W::Error>> {
            encoder.begin_map()?;
            let idx = task.index();
            encoder.str("task")?;
            // Prefer the string task name, provided that the the task isn't out
            // of range (which would be weird and bad, but we may as well still
            // report it).
            //
            // We could make the ereport more concise by using task indices
            // rather than the whole string, but we expect fault ereports are
            // likelier to be read by a human being and making them
            // interpretable without direct access to the Hubris archive is
            // useful.
            match hubris_task_names::TASK_NAMES.get(idx) {
                Some(name) => encoder.str(name)?,
                None => encoder.encode(idx)?,
            };
            encoder.str("gen")?.u8(task.generation().into())?;
            encoder.end()?;
            Ok(())
        }

        fn encode_fault_src<W: encode::Write>(
            encoder: &mut minicbor::Encoder<W>,
            source: FaultSource,
        ) -> Result<(), encode::Error<W::Error>> {
            encoder.str("src")?;
            match source {
                FaultSource::Kernel => encoder.str("kern")?,
                FaultSource::User => encoder.str("usr")?,
            };
            Ok(())
        }

        ringbuf_entry!(Trace::TaskFaulted { task, nfaults });

        let cursor = encode::write::Cursor::new(buf);
        let mut encoder = minicbor::Encoder::new(cursor);
        encoder.begin_map()?;
        // Ereport version.
        encoder.str("v")?.u32(0)?;

        // If the task has faulted multiple times since the last ereport we
        // generated for it, record the count.
        if nfaults > 1 {
            encoder.str("nfaults")?.u32(nfaults as u32)?;
        }

        // If we are able to read the faulted task's status, record a more
        // detailed ereport.
        if let TaskState::Faulted { fault, .. } =
            kipc::read_task_status(task.index())
        {
            match fault {
                FaultInfo::MemoryAccess { address, source } => {
                    encoder.str("k")?.str("hubris.fault.mem_access")?;
                    encoder.str("addr")?.encode(address)?;
                    encode_fault_src(&mut encoder, source)?;
                }
                FaultInfo::StackOverflow { address } => {
                    encoder.str("k")?.str("hubris.fault.stack")?;
                    encoder.str("addr")?.encode(address)?;
                }
                FaultInfo::BusError { address, source } => {
                    encoder.str("k")?.str("hubris.fault.bus")?;
                    encoder.str("addr")?.encode(address)?;
                    encode_fault_src(&mut encoder, source)?;
                }
                FaultInfo::DivideByZero => {
                    encoder.str("k")?.str("hubris.fault.div_0")?;
                }
                FaultInfo::IllegalText => {
                    encoder.str("k")?.str("hubris.fault.illegal_txt")?;
                }
                FaultInfo::IllegalInstruction => {
                    encoder.str("k")?.str("hubris.fault.illegal_inst")?;
                }
                FaultInfo::InvalidOperation(code) => {
                    encoder.str("k")?.str("hubris.fault.invalid_op")?;
                    encoder.str("code")?.u32(code)?;
                }
                FaultInfo::SyscallUsage(err) => {
                    encoder.str("k")?.str("hubris.fault.syscall")?;
                    encoder.str("err")?;
                    // These strings are kind of a lot of characters, but the rest
                    // of the ereport is short and it seems kinda helpfulish to use
                    // the same names as the actual enum variants, so they're
                    // greppable in the source code.
                    //
                    // Also, keeping them in CamelCase makes them a few characters
                    // shorter than converting them to snake_case, since there
                    // aren't any underscores. Which...kind of flies in the face of
                    // my previous paragraph saying that we're not trying to make
                    // them shorter to save on bytes of CBOR, but...
                    //
                    // Using `minicbor_serde` just to encode the enums as strings
                    // felt a bit too heavyweight, and required wrapping the encoder
                    // in a serde thingy, so...we're doing it the old fashioned way.
                    encoder.str(match err {
                        UsageError::BadSyscallNumber => "BadSyscallNumber",
                        UsageError::InvalidSlice => "InvalidSlice",
                        UsageError::TaskOutOfRange => "TaskOutOfRange",
                        UsageError::IllegalTask => "IllegalTask",
                        UsageError::LeaseOutOfRange => "LeaseOutOfRange",
                        UsageError::OffsetOutOfRange => "OffsetOutOfRange",
                        UsageError::NoIrq => "NoIrq",
                        UsageError::BadKernelMessage => "BadKernelMessage",
                        UsageError::BadReplyFaultReason => {
                            "BadReplyFaultReason"
                        }
                        UsageError::NotSupervisor => "NotSupervisor",
                        UsageError::ReplyTooBig => "ReplyTooBig",
                    })?;
                }
                FaultInfo::Panic => {
                    encoder.str("k")?.str("hubris.fault.panic")?;
                    encoder.str("msg")?;
                    match kipc::read_panic_message(task.index(), panic_buf) {
                        Ok(msg_chunks) => {
                            encoder.begin_str()?;
                            for chunk in msg_chunks {
                                let valid = chunk.valid();

                                // avoid a big pile of 0-length strings
                                if !valid.is_empty() {
                                    encoder.str(valid)?;
                                }

                                if !chunk.invalid().is_empty() {
                                    // oh, there's also some trash in here!
                                    encoder.str("�")?;
                                }
                            }
                            encoder.end()?;
                        }
                        Err(ReadPanicMessageError::TaskNotPanicked) => {
                            ringbuf_entry!(Trace::MissedPanicMessage { task });
                            encoder.null()?;
                        }
                        Err(ReadPanicMessageError::BadPanicBuffer) => {
                            ringbuf_entry!(Trace::BadPanicMessage { task });
                            encoder.null()?;
                        }
                    }
                }
                FaultInfo::Injected(by_task) => {
                    encoder.str("k")?.str("hubris.fault.injected")?;
                    encoder.str("by")?;
                    encode_task(&mut encoder, by_task)?;
                }
                FaultInfo::FromServer(srv_task, err) => {
                    encoder.str("k")?.str("hubris.fault.from_srv")?;
                    encoder.str("srv")?;
                    encode_task(&mut encoder, srv_task)?;
                    encoder.str("err")?;
                    // These strings are kind of a lot of characters, but the rest
                    // of the ereport is short and it seems kinda helpfulish to use
                    // the same names as the actual enum variants, so they're
                    // greppable in the source code.
                    //
                    // Also, keeping them in CamelCase makes them a few characters
                    // shorter than converting them to snake_case, since there
                    // aren't any underscores. Which...kind of flies in the face of
                    // my previous paragraph saying that we're not trying to make
                    // them shorter to save on bytes of CBOR, but...
                    //
                    // Using `minicbor_serde` just to encode the enums as strings
                    // felt a bit too heavyweight, and required wrapping the encoder
                    // in a serde thingy, so...we're doing it the old fashioned way.
                    encoder.str(match err {
                        ReplyFaultReason::UndefinedOperation => {
                            "UndefinedOperation"
                        }
                        ReplyFaultReason::BadMessageSize => "BadMessageSize",
                        ReplyFaultReason::BadMessageContents => {
                            "BadMessageContent"
                        }
                        ReplyFaultReason::BadLeases => "BadLeases",
                        ReplyFaultReason::ReplyBufferTooSmall => {
                            "ReplyBufferTooSmall"
                        }
                        ReplyFaultReason::AccessViolation => "AccessViolation",
                    })?;
                }
            };
        } else {
            // Welp. By the time we managed to read the faulted task's status,
            // it has already been restarted.
            //
            // In this case, we should still generate an ereport indicating
            // that there was a fault, even if we can't say which one it was.
            encoder.str("k")?.str("hubris.fault.unknown")?;
            ringbuf_entry!(Trace::TaskAlreadyRecovered { task });
        }
        encoder.end()?;

        let cursor = encoder.into_writer();
        let len = cursor.position();
        let buf = cursor.into_inner();

        Ok(&buf[..len])
    }
}

impl EreportBufs {
    pub(crate) const fn new() -> Self {
        Self {
            storage: snitch_core::Store::DEFAULT,
            recv: [0u8; RECV_BUF_SIZE],
            panic_buf: [0u8; userlib::PANIC_MESSAGE_MAX_LEN],
            task_gens: [TaskHistory {
                last_gen: 0,
                tfaulted: None,
            }; hubris_num_tasks::NUM_TASKS],
        }
    }
}

struct ByteGather<'a, 'b>(&'a [u8], &'b [u8]);

impl<C> minicbor::Encode<C> for ByteGather<'_, '_> {
    fn encode<W: encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
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
