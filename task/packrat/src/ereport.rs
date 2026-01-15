// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! # Packrat ereport aggregation.
//!
//! As described in [RFD 545 § 4.3], `packrat`'s role in the ereport subsystem
//! is to aggregate ereports from other tasks in a circular buffer. Ereports are
//! submitted to `packrat` via the `deliver_ereport` IPC call. The `snitch` task
//! requests ereports from `packrat` using the `read_ereports` IPC call, which
//! also flushes committed ereports from the buffer.
//!
//! [RFD 545 § 4.3]: https://rfd.shared.oxide.computer/rfd/0545#_aggregation
//!
//! ## Fault Reporting
//!
//! In addition to aggregating ereports recorded by other tasks, this module is
//! also responsible for recording ereports when a task faults. This is
//! `packrat`'s responsibility, since the faulted task cannot submit an ereport
//! to `packrat` because, well, you know...it just faulted,[^1] and making it
//! the supervisor's duty to send ereports to `packrat` feels like putting too
//! much code in the supervisor.
//!
//! To provide the most detailed diagnostic information about the fault, some
//! data must be read while the task is still in the faulted state, prior to
//! restarting it. In particular, the [`FaultInfo`] provided by the kernel in
//! response to [`kipc::read_task_status`] is discarded when the task is
//! restarted (as its state transitions back to [`TaskState::Running`]), and if
//! the task has panicked, the panic message is read directly out of its address
//! space via [`kipc::read_panic_message`], and this is also clobbered when the
//! task is restarted. However, we do not want a broken `packrat` to be able to
//! delay restarting faulted tasks indefinitely, so we have only a limited time
//! window to read this data before the task is restarted. In the event that we
//! miss our chance to do so, or if the ereport buffer is full at the time of
//! the fault, we will still attempt to produce a less-detailed ereport
//! indicating that there has been *some* kind of fault.
//!
//! The overall theory of operation for task fault ereports is:
//!
//! 1. `jefe` is configured to send the `TASK_FAULTED` notification to `packrat`
//!    whenever a task has faulted.
//!
//!    When the list of tasks to notify on faults is non-empty, `jefe` will
//!    always wait at least 5 ms before restarting the faulted task,
//!    regardless of how long it has been running prior to the fault. This
//!    should be sufficient time for `packrat` to record data about the fault,
//!    as described above.
//!
//! 2. When we receive the `TASK_FAULTED` notification from the supervisor,
//!    `packrat` calls the [`Jefe::read_task_fault_counts`] IPC to ask `jefe`
//!    for the total number of times each task has faulted since boot. We
//!    scan over this array (in [`EreportStore::record_faulted_tasks`]) and
//!    compare the fault counts from `jefe` to the number of faults we have
//!    previously observed for each task, to detect any unrecorded faults.
//!
//!    We ask `jefe` for faulted tasks, rather than using
//!    [`kipc::find_faulted_task`], because a faulted task may have already
//!    been restarted by the time we get around to trying to record it.
//!    Similarly, we ask `jefe` for the total fault counts since boot, rather
//!    than just saying "give me the last faulted task", because it is possible
//!    for multiple tasks to have faulted, or the same task to fault multiple
//!    times[^2], since the last time we had an opportunity to check for
//!    faulted tasks.
//!
//! 3. If we find a task whose fault count has changed, we call
//!    [`EreportStore::record_faulted_task`] to produce an ereport for that
//!    fault. If the task is still in the faulted state (which, again, should be
//!    the common case due to `packrat`'s relative priority), this will be a more
//!    detailed ereport than if it has already been restarted, but if the task
//!    has been restarted, we will still indicate that some sort of fault
//!    occurred.
//!
//! 4. If the ereport produced for a faulted task fits in the ereport buffer,
//!    we update `packrat`'s tracked fault count to indicate that the fault has
//!    been recorded. If there is *not* currently space to record the fault, we
//!    instead track the timestamp at which the fault occurred in the tracked
//!    fault history for that task, and set a flag indicating that we are
//!    currently "holding" un-reported faults.
//!
//! 5. When ereports are flushed from the buffer, if the "holding faults" flag
//!    is set, we call [`EreportStore::record_faulted_tasks`] again. This way,
//!    if any task faults were previously not able to fit in the buffer, we
//!    attempt to record them again now that there's space. By this point, the
//!    task will *probably* have been restarted, so the ereport will be less
//!    detailed, but again, indicating that a fault occurred at all is still
//!    better than not doing that.
//!
//! [^1]: While we could imagine the panic hook producing an ereport *prior* to
//!       invoking the `sys_panic` syscall, other faults such as stack
//!       overflows do  not provide similar opportunities for a task to
//!       report on its own misbehavior.
//! [^2]: Though the combination of the 50 ms restart delay in `jefe` and the
//!       fact that `packrat` is usually immediately below the supervisor in
//!       priority makes it *unlikely* that the same task will have faulted
//!       a bunch of times before we see it, it's always possible.

use super::ereport_messages;

use core::cmp::Ordering;
use drv_caboose::CabooseReader;
use hubris_num_tasks::NUM_TASKS;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use minicbor::{encode, CborLen};
use minicbor_lease::LeasedWriter;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_jefe_api::Jefe;
use task_packrat_api::{EreportReadError, EreportWriteError, OxideIdentity};
use userlib::{
    kipc, sys_get_timer, task_slot, FaultInfo, FaultSource, Generation,
    ReadPanicMessageError, RecvMessage, ReplyFaultReason, TaskId, TaskState,
    UsageError,
};
use zerocopy::IntoBytes;

task_slot!(JEFE, jefe);

pub(crate) struct EreportStore {
    storage: &'static mut snitch_core::Store<STORE_SIZE>,
    recv: &'static mut [u8; RECV_BUF_SIZE],
    pub(super) restart_id: Option<ereport_messages::RestartId>,
    //
    // === Stuff for task fault reporting ===
    //
    /// Buffer into which we read panic messages when preparing a fault report
    /// for a panicked task. Thankfully, the userlib tells us the maximum
    /// length (in bytes) that these will be.
    panic_buf: &'static mut [u8; userlib::PANIC_MESSAGE_MAX_LEN],
    jefe: Jefe,
    /// Tracks the last observed fault count for each task in the image, and
    /// the timestamp of the last **unrecorded** fault, if there is one.
    ///
    /// This is used to determine whether a task has faulted when Jefe notifies
    /// us of task faults, and whether there are unrecorded faults when we free
    /// up buffer space for more ereports.
    task_fault_states: &'static mut [TaskFaultHistory; NUM_TASKS],
    /// ...and this one is the buffer that we mutably lease to Jefe when we ask
    /// it to give us the _latest_ fault counts.
    fault_count_buf: &'static mut [usize; NUM_TASKS],
    /// Set when we are notified of task fault(s) by, Jefe, but there is
    /// insufficient space in the ereport buffer to record a fault ereport.
    ///
    /// If this is set, we will attempt to record a (less detailed) ereport for
    /// the task fault later, when space is freed up.
    holding_faults: bool,
}

pub(crate) struct EreportBufs {
    storage: snitch_core::Store<STORE_SIZE>,
    recv: [u8; RECV_BUF_SIZE],
    panic_buf: [u8; userlib::PANIC_MESSAGE_MAX_LEN],
    task_fault_states: [TaskFaultHistory; NUM_TASKS],
    fault_count_buf: [usize; NUM_TASKS],
}

/// Per-task state for fault reporting.
#[derive(Copy, Clone)]
struct TaskFaultHistory {
    /// The total number of faults since boot *for which we have successfully
    /// produced an ereport*.
    ///
    /// This value is compared with the value returned by
    /// [`Jefe::read_fault_counts`] to detect un-recorded task faults. It is
    /// updated to the value returned by `jefe` when a fault ereport is
    /// successfully inserted into the storage buffer.
    count: usize,
    /// The timestamp of the last time this task faulted but an ereport did
    /// *not* fit in the ereport buffer. If this is set, it indicates that the
    /// task one or more has unrecorded faults.
    ///
    /// This is used as the timestamp of the fault report for those unreported
    /// faults when space becomes available.
    last_unrecorded_fault_time: Option<u64>,
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
    #[count(skip)]
    HoldingFaults(bool),
    FaultRecorded {
        task_index: u16,
        #[count(children)]
        result: snitch_core::InsertResult,
        len: usize,
    },
    TaskFaulted {
        task_index: u16,
        nfaults: usize,
    },
    // A fault report was >1024B long! what the heck!
    GiantFaultReport {
        task_index: u16,
    },
    MissedPanicMessage {
        task_index: u16,
    },
    BadPanicMessage {
        task_index: u16,
    },
    TaskAlreadyRecovered {
        task_index: u16,
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
            ref mut task_fault_states,
            ref mut fault_count_buf,
        }: &'static mut EreportBufs,
    ) -> Self {
        let now = sys_get_timer().now;
        storage.initialize(config::TASK_ID, now);

        Self {
            storage,
            recv,
            panic_buf,
            task_fault_states,
            holding_faults: false,
            restart_id: None,
            fault_count_buf,
            jefe: Jefe::from(JEFE.get_task_id()),
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
        // Either Jefe has sent us a notification of a fault, or we have just
        // made some more room in our buffer and could potentially record a
        // previously held fault.
        //
        // In either case, begin by asking Jefe for the current task fault
        // counters.
        self.jefe.read_fault_counts(self.fault_count_buf);
        let mut nfaulted: usize = 0;
        let mut nreported: usize = 0;

        // Who farted^Wfaulted?
        for (task_index, (state, new_count)) in self
            .task_fault_states
            .iter_mut()
            .zip(self.fault_count_buf.iter().copied())
            .enumerate()
        {
            let task_index = task_index as u16;
            let TaskFaultHistory {
                ref mut count,
                ref mut last_unrecorded_fault_time,
            } = state;

            // Check if the fault count has changed to determine whether there
            // are new faults. At the same time, check if we are "holding onto"
            // previously observed faults for this task. This is also how we
            // determine the timestamp for the fault report: either it is a
            // previously held timestamp if the task has unreported faults AND
            // no new faults have occurred, or it is the "now" time if new
            // faults have occurred.
            //
            // This bit looks a little weird, but it'll make sense if you think
            // about it...trust me!
            let (nfaults, timestamp) =
                match (new_count.cmp(&*count), *last_unrecorded_fault_time) {
                    // If the new fault count is less than the current count,
                    // then Jefe's counter has wrapped around. The number of
                    // times the task has faulted is the difference between the
                    // prior count and `usize::MAX`, plus the new fault count.
                    //
                    // This is a bit fudgey if the fault counter has wrapped
                    // multiple times since the last we saw it, but there's no
                    // good way to detect that. Also, it seems basically
                    // impossible for a task to have faulted more than
                    // `u32::MAX` times between packrat being scheduled,
                    // especially considering the 50ms restart cooldown between
                    // successive faults...
                    (Ordering::Less, _) => {
                        let nfaults = usize::MAX
                            .saturating_sub(*count)
                            .saturating_add(new_count);
                        (nfaults, now)
                    }
                    // Task has not faulted, so just move on to the next one.
                    //
                    // N.B. that if the counter has wrapped back to *exactly*
                    // the same value it was last time we checked, this *could*
                    // be wrong, but...man, that just seems wildly unlikely. In
                    // practice, we worry about the counter wrapping *since
                    // boot* and not *since the last time we looked at it*.
                    (Ordering::Equal, None) => continue,
                    // No *new* faults have occurred, but we are holding a
                    // previously observed fault for this task that was not
                    // successfully reported.
                    //
                    // This case actually *shouldn't happen*, since we only
                    // update the tracked count if the faults are successfully
                    // recorded. But handle it gracefully anyway.
                    (Ordering::Equal, Some(t)) => (1, t),
                    // The counter has increased, so the number of faults we
                    // haven't seen is just the difference between the new and
                    // current counts.
                    (Ordering::Greater, _) => {
                        let nfaults = new_count.saturating_sub(*count);
                        (nfaults, now)
                    }
                };

            // This will never wrap, since there can't be more than
            // `hubris_num_tasks::NUM_TASKS` tasks that have faulted, but the
            // compiler doesn't know this.
            nfaulted = nfaulted.wrapping_add(1);
            ringbuf_entry!(Trace::TaskFaulted {
                task_index,
                nfaults
            });

            if let Ok((ereport, taskid)) = Self::record_faulted_task(
                &mut self.recv[..],
                self.panic_buf,
                task_index,
                nfaults,
            ) {
                let result = self.storage.insert(taskid.0, timestamp, ereport);
                ringbuf_entry!(Trace::FaultRecorded {
                    task_index,
                    result,
                    len: ereport.len()
                });
                match result {
                    snitch_core::InsertResult::Inserted => {
                        // We successfully made an ereport for this fault!
                        // Update our tracked fault count for this task.
                        *count = new_count;
                        // Again, won't ever actually wrap, but whatever.
                        nreported = nreported.wrapping_add(1);
                        // All previously observed faults recorded!
                        *last_unrecorded_fault_time = None;
                    }
                    snitch_core::InsertResult::Lost => {
                        // No ereport was recorded, so *don't* acknowledge the
                        // fault by updating our tracked fault count. This way
                        // we will still treat the task as having faulted in the
                        // past and will attempt to make an ereport for it
                        // later, if there's space.
                        *last_unrecorded_fault_time = Some(timestamp);
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
                ringbuf_entry!(Trace::GiantFaultReport { task_index });
                // Treat the fault as acked, because if it was >1024B this
                // time, it will always be >1024B next time.
                *count = new_count;
                *last_unrecorded_fault_time = None;
                // Again, won't ever actually wrap, but whatever.
                nreported = nreported.wrapping_add(1);
            };
        }

        // If we successfully recorded an ereport for every task we observed to
        // have faulted, we are no longer "holding" unreported faults.
        self.holding_faults = nreported != nfaulted;
        ringbuf_entry!(Trace::HoldingFaults(self.holding_faults));
    }

    /// Record an ereport indicating that a Hubris task has faulted.
    ///
    /// Ereports for hardware faults are largely intended to be interpreted by
    /// the automated fault-management system. The Hubris task ereports we
    /// generate here, on the other hand, generally represent a firmware bug
    /// rather than an anticipated hardware failure, and therefore, we expect
    /// that it is much likelier that the ereport will be read by a human
    /// being.
    ///
    /// Thus, we err on the side of human-readability somewhat with their
    /// contents.
    //
    // This is, somewhat sadly, not a method, as it must mutably borrow some of
    // the buffers whilst other parts of `self` are borrowed in the loop over
    // possibly-faulted task fault counts.
    fn record_faulted_task<'buf>(
        buf: &'buf mut [u8],
        panic_buf: &mut [u8; userlib::PANIC_MESSAGE_MAX_LEN],
        task_index: u16,
        nfaults: usize,
    ) -> Result<(&'buf [u8], TaskId), encode::Error<encode::write::EndOfSlice>>
    {
        /// Encode a CBOR object representing another task that was involved in
        /// a fault; either the injecting task in a `FaultInfo::Injected`, or
        /// the server that responded with a `REPLY_FAULT` in a
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
            // Prefer the string task name, provided that the the task isn't
            // out of range (which would be weird and bad, but we may as well
            // still report it).
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

        // The generation to include in the ereport will depend on whether the
        // task is currently in the faulted state, or if it has already been
        // restarted. If it has been restarted, we will record the ereport with
        // the current generation minus 1, since that was the generation at
        // which the fault occurred.
        let taskid =
            TaskId::for_index_and_gen(task_index as usize, Generation::ZERO);
        let mut taskid = userlib::sys_refresh_task_id(taskid);

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
            kipc::read_task_status(task_index as usize)
        {
            match fault {
                FaultInfo::MemoryAccess { address, source } => {
                    encoder.str("k")?.str("hubris.fault.mem")?;
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
                    encoder.str("k")?.str("hubris.fault.div0")?;
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
                    // These strings are kind of a lot of characters, but the
                    // rest of the ereport is short and it seems kinda
                    // helpfulish to use the same names as the actual enum
                    // variants, so they're greppable in the source code.
                    //
                    // Also, keeping them in CamelCase makes them a few
                    // characters shorter than converting them to snake_case,
                    // since there aren't any underscores. Which...kind of flies
                    // in the face of my previous paragraph saying that we're
                    // not trying to make them shorter to save on bytes of CBOR,
                    // but...
                    //
                    // Using `minicbor_serde` just to encode the enums as
                    // strings felt a bit too heavyweight, and required wrapping
                    // the encoder in a serde thingy, so...we're doing it the
                    // old fashioned way.
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
                    match kipc::read_panic_message(
                        task_index as usize,
                        panic_buf,
                    ) {
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
                            ringbuf_entry!(Trace::MissedPanicMessage {
                                task_index
                            });
                            encoder.null()?;
                        }
                        Err(ReadPanicMessageError::BadPanicBuffer) => {
                            ringbuf_entry!(Trace::BadPanicMessage {
                                task_index
                            });
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
                    encoder.str("k")?.str("hubris.fault.reply")?;
                    encoder.str("srv")?;
                    encode_task(&mut encoder, srv_task)?;
                    encoder.str("err")?;
                    // These strings are kind of a lot of characters, but the
                    // rest of the ereport is short and it seems kinda
                    // helpfulish to use the same names as the actual enum
                    // variants, so they're greppable in the source code.
                    //
                    // Also, keeping them in CamelCase makes them a few
                    // characters shorter than converting them to snake_case,
                    // since there aren't any underscores. Which...kind of flies
                    // in the face of my previous paragraph saying that we're
                    // not trying to make them shorter to save on bytes of CBOR,
                    // but...
                    //
                    // Using `minicbor_serde` just to encode the enums as
                    // strings felt a bit too heavyweight, and required wrapping
                    // the encoder in a serde thingy, so...we're doing it the
                    // old fashioned way.
                    encoder.str(match err {
                        ReplyFaultReason::UndefinedOperation => {
                            "UndefinedOperation"
                        }
                        ReplyFaultReason::BadMessageSize => "BadMessageSize",
                        ReplyFaultReason::BadMessageContents => {
                            "BadMessageContents"
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
            encoder.str("k")?.str("hubris.fault")?;
            ringbuf_entry!(Trace::TaskAlreadyRecovered { task_index });
            // If the task has already restarted, we must decrement the
            // reported generation for the ereport by 1, so that we record the
            // generation that faulted, rather than the current one.
            let generation = u8::from(taskid.generation()).wrapping_sub(1);
            taskid = TaskId::for_index_and_gen(
                task_index as usize,
                Generation::from(generation),
            );
        }
        encoder.end()?;

        let cursor = encoder.into_writer();
        let len = cursor.position();
        let buf = cursor.into_inner();

        Ok((&buf[..len], taskid))
    }
}

impl EreportBufs {
    pub(crate) const fn new() -> Self {
        Self {
            storage: snitch_core::Store::DEFAULT,
            recv: [0u8; RECV_BUF_SIZE],
            panic_buf: [0u8; userlib::PANIC_MESSAGE_MAX_LEN],
            task_fault_states: [TaskFaultHistory {
                count: 0,
                last_unrecorded_fault_time: None,
            }; NUM_TASKS],
            fault_count_buf: [0usize; NUM_TASKS],
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
