// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![cfg_attr(not(test), no_std)]

use core::num::NonZeroU32;

use unwrap_lite::UnwrapLite as _;

/// A fixed-size store for ereports.
///
/// A `Store<N>` stores up to `N` bytes of ereports and tracks data loss. (Note
/// that this type currently imposes 12 bytes of overhead per ereport, so take
/// that into account when choosing `N`.)
///
/// # Internal record format
///
/// Internally, records are treated as unparsed blobs, wrapped with a basic
/// header.
///
/// This header is never exposed to clients and has no forward compatibility
/// requirement, but is important for the internals.
///
/// The header consists of:
///
/// - The number of bytes in the record, as a little-endian 16-bit integer.
/// - The originating `TaskId`, as a little-endian 16-bit integer.
/// - The system uptime when the record was received, represented as a
///   little-endian 64-bit integer.
///
/// All fields in the header are _unaligned_ in memory to avoid wasting space on
/// padding. This means the overhead for a record is always 12 bytes.
#[derive(Clone, Debug)]
pub struct Store<const N: usize> {
    storage: heapless::Deque<u8, N>,

    /// The ENA (ereport identifier) of the _oldest_ record currently in
    /// storage. Because we issue ENAs consectively, this avoids the need to
    /// spend 8 bytes per record storing them.
    ///
    /// As a result, it's critical to update this whenever data is popped.
    ///
    /// When the queue is empty (including when the store is created) this field
    /// contains the ENA that _will be_ assigned to the next pushed record.
    earliest_ena: u64,

    /// Current state of the record insertion state machine. See the enum for
    /// details.
    insert_state: InsertState,

    /// To keep this crate platform-independent (for testing) and to keep this
    /// type in bss, we expect the task that creates a `Store` to provide its
    /// task ID to `initialize`; it then gets stored here. (This structure needs
    /// to know its owner's task ID because it gets inserted when we generate a
    /// loss record.)
    our_task_id: u16,

    /// Number of records stored in `storage` right now.
    ///
    /// This is used to screen incoming ENAs for validity: only ENAs
    /// `earliest_ena .. earliest_ena+stored_record_count` are valid.
    stored_record_count: usize,
}

impl<const N: usize> Store<N> {
    /// Empty store constant for use in static initializers.
    ///
    /// This constant is designed to evaluate to all-zeroes, ensuring that the
    /// static goes in bss rather than initialized data.
    pub const DEFAULT: Self = Self {
        storage: heapless::Deque::new(),
        earliest_ena: 0,
        insert_state: InsertState::Collecting,
        our_task_id: 0,
        stored_record_count: 0,
    };

    /// Sets up the `Store` with data from the environment. Must be called once
    /// before other functions.
    ///
    /// This stores the bits of the current task's `TaskId` (`tid`) and the
    /// current system `timestamp`, to be used in generating loss records.
    pub fn initialize(&mut self, tid: u16, timestamp: u64) {
        self.our_task_id = tid;
        // If the queue has never been touched, insert our "arbitrary data loss"
        // record into the stream to consume ENA 0.
        if !self.initialized() {
            // Should always succeed...
            let _ = self.insert_impl(
                self.our_task_id,
                timestamp,
                // This is a canned CBOR message that decodes as...
                // a1  # map(1)
                //   # key
                //   64 6c 6f 73 74  # text("lost")
                //   # value
                //   f6  # null / None
                &[0xA1, 0x64, 0x6C, 0x6F, 0x73, 0x74, 0xF6],
            );
            // ENAs start at 1.
            self.earliest_ena = 1;
        }
    }

    /// Internal check for whether `initialize` has been called.
    fn initialized(&self) -> bool {
        self.earliest_ena != 0 || !self.storage.is_empty()
    }

    /// Returns the current free space in the queue, in bytes. This is raw;
    /// subtract 12 to get the largest single message that can be enqueued.
    pub fn free_space(&self) -> usize {
        match self.insert_state {
            InsertState::Collecting => N - self.storage.len(),
            InsertState::Losing { .. } => {
                // Indicate how much space we have after recovery succeeds,
                // which may be zero if we can't recover yet.
                (N - self.storage.len())
                    .saturating_sub(OVERHEAD)
                    .saturating_sub(DATA_LOSS_LEN)
            }
        }
    }

    /// Inserts a record, or records it as lost.
    ///
    /// This attempts to record a record from `sender` at `timestamp` containing
    /// `data`. If the record can't be stored, this records it as lost. No
    /// indication is given to the caller, since the caller can't really do
    /// anything anyway.
    ///
    /// If we're already in a `Losing` situation, but data has been flushed such
    /// that we might yet be able to make forward progress, this will attempt to
    /// recover before inserting this record.
    ///
    /// Note that there is a maximum record size supported by this
    /// implementation. This maximum size is larger than we expect our backing
    /// buffer to be. Any records larger than the max will be treated as not
    /// fitting. (Currently the max is 64 kiB.)
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the record was successfully inserted.
    /// - `Err(())` if the record was lost due to insufficient space.
    pub fn insert(
        &mut self,
        sender: u16,
        timestamp: u64,
        data: &[u8],
    ) -> Result<(), ()> {
        debug_assert!(self.initialized());
        self.insert_impl(sender, timestamp, data)
    }

    /// Iterates over the entire current contents of the store.
    ///
    /// This will produce `Record`s in the order they were received, with
    /// ascending ENA values.
    pub fn iter_contents(&mut self) -> impl Iterator<Item = Record<'_>> + '_ {
        // Attempt to recover and insert a loss record if necessary. If the next
        // record that arrives is large, and we're short on space, this may
        // cause us to generate a sequence of loss records -- but it currently
        // seems more important to admit the loss to the caller, so that they
        // might flush it, than to try and special case that.
        self.recover_if_required(None);

        let mut slices = self.storage.as_slices();
        let mut next_ena = self.earliest_ena;
        core::iter::from_fn(move || {
            let len = u16::from_le_bytes(take_array(&mut slices)?) as usize;
            let tid = u16::from_le_bytes(take_array(&mut slices)?);
            let timestamp = u64::from_le_bytes(take_array(&mut slices)?);
            let slices = take_slice(&mut slices, len)?;
            let ena = next_ena;
            next_ena += 1;
            Some(Record {
                ena,
                tid,
                timestamp,
                slices,
            })
        })
    }

    /// Reads records starting from `first_ena`, inclusive.
    pub fn read_from(
        &mut self,
        first_ena: u64,
    ) -> impl Iterator<Item = Record<'_>> + '_ {
        // We can use `skip_while` instead of `filter` here because the ENAs are
        // always ascending.
        self.iter_contents()
            .skip_while(move |rec| rec.ena < first_ena)
    }

    /// Discards records up through and including `last_written_ena`.
    ///
    /// This is intended to be called when we've received confirmation that a
    /// range of records has been committed to a database. The provided ENA is
    /// _inclusive_ and indicates the last record written to the database.
    ///
    /// If the ENA is either lower than any of our stored records, or higher
    /// than what we've vended out, this is a no-op.
    pub fn flush_thru(&mut self, last_written_ena: u64) {
        let Some(index) = last_written_ena.checked_sub(self.earliest_ena)
        else {
            // Cool, we're already aware that record has been written.
            return;
        };
        if index >= self.stored_record_count as u64 {
            // Uhhhh. We have not issued this ENA. It could not possibly have
            // been written.
            //
            // TODO: is this an opportunity for the queue _itself_ to generate
            // a defect report? For now, we'll just ignore it.
            return;
        }

        for _ in 0..=index {
            // Discard the lead record in the queue.
            let mut slices = self.storage.as_slices();
            let len = u16::from_le_bytes(take_array(&mut slices).unwrap_lite())
                as usize;

            let size = OVERHEAD + len;
            // So it's weird, but, heapless::Deque only lets you pop individual
            // bytes. Hopefully this is not too expensive.
            for _ in 0..size {
                self.storage.pop_front();
            }

            self.stored_record_count -= 1;
            self.earliest_ena += 1;
        }

        // You might be curious why we don't do our loss recovery process here,
        // and instead do it when the next record is received. The goal is to
        // avoid generating a sequence of loss events when the queue is very
        // full. If we are able to recover here, but do _not_ have enough bytes
        // free for the next incoming record, then we'd just kick back into loss
        // state.... necessitating another loss record. And so forth.
    }

    /// Makes a best effort at inserting a record.
    ///
    /// This will attempt to recover from the `Losing` state before writing
    /// anything.
    ///
    /// If, after recovery, we are `Collecting` and there is enough space for a
    /// header (`OVERHEAD`) plus `data`, the record is stored, implicitly
    /// assigned the next ENA.
    ///
    /// If recovery is not possible, the existing loss count is advanced.
    ///
    /// If we are `Collecting` but `data` plus a header won't fit, we enter the
    /// `Losing` state with a count of 1.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the record was successfully inserted.
    /// - `Err(())` if the record was lost due to insufficient space.
    fn insert_impl(
        &mut self,
        sender: u16,
        timestamp: u64,
        data: &[u8],
    ) -> Result<(), ()> {
        // We attempt recovery here so that we can _avoid_ generating a loss
        // record if this next record (`data`) is just going to kick us back
        // into loss state.
        self.recover_if_required(Some(OVERHEAD + data.len()));

        let data_len = u16::try_from(data.len()).ok();

        match &mut self.insert_state {
            InsertState::Collecting => {
                let room = self.storage.capacity() - self.storage.len();
                if data_len.is_some_and(|n| room >= OVERHEAD + n as usize) {
                    self.write_header(
                        data_len.unwrap_lite(),
                        sender,
                        timestamp,
                    );
                    for &byte in data {
                        self.storage.push_back(byte).unwrap_lite();
                    }
                    Ok(())
                } else {
                    self.insert_state = InsertState::Losing {
                        count: NonZeroU32::new(1).unwrap_lite(),
                        timestamp,
                    };
                    Err(())
                }
            }
            InsertState::Losing { count, .. } => {
                *count = count.saturating_add(1);
                Err(())
            }
        }
    }

    /// Internal utility routine for storing a record header.
    fn write_header(&mut self, data_len: u16, task: u16, timestamp: u64) {
        for byte in data_len.to_le_bytes() {
            self.storage.push_back(byte).unwrap_lite();
        }
        for byte in task.to_le_bytes() {
            self.storage.push_back(byte).unwrap_lite();
        }
        for byte in timestamp.to_le_bytes() {
            self.storage.push_back(byte).unwrap_lite();
        }
        self.stored_record_count += 1;
    }

    /// Checks if we're losing data and attempts to stop, by generating a loss
    /// record in the queue.
    fn recover_if_required(&mut self, space_required: Option<usize>) {
        // We only need to take action if we're in Losing state.
        if let InsertState::Losing { count, timestamp } = self.insert_state {
            // Note: already includes OVERHEAD/DATA_LOSS_LEN
            let room = self.free_space();
            let required = space_required.unwrap_or(0);
            if room >= required {
                // We can recover!
                self.write_header(
                    DATA_LOSS_LEN as u16,
                    self.our_task_id,
                    timestamp,
                );

                // CBOR-format loss record:
                // a1  # map(1)
                //    # key
                //   64 6c 6f 73 74  # text("lost")
                //    # value
                //   1a 00 00 00 00

                for byte in [0xA1, 0x64, 0x6C, 0x6F, 0x73, 0x74, 0x1A] {
                    self.storage.push_back(byte).unwrap_lite();
                }
                for byte in u32::from(count).to_be_bytes() {
                    self.storage.push_back(byte).unwrap_lite();
                }

                self.insert_state = InsertState::Collecting;
            } else {
                // Recovery is not currently possible (not enough room).
            }
        } else {
            // We're good.
        }
    }
}

/// Represents a record in the queue, with its header parsed.
///
/// The body of the record is given as a _pair_ of slices because it's not
/// necessarily contiguous in the internal deque.
#[derive(Copy, Clone, Debug)]
pub struct Record<'s> {
    /// The event's ENA.
    pub ena: u64,
    /// The sender's task ID.
    pub tid: u16,
    /// The timestamp from the report.
    pub timestamp: u64,
    /// The contents of the report.
    pub slices: (&'s [u8], &'s [u8]),
}

impl Record<'_> {
    /// Iterates over the body bytes, hiding the fact that they be
    /// discontiguous.
    pub fn body_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.slices.0.iter().chain(self.slices.1.iter()).copied()
    }
}

/// Utility function for pulling a fixed number of bytes from the front of a
/// pair of slices.
fn take_array<'a, const N: usize>(
    slices: &mut (&'a [u8], &'a [u8]),
) -> Option<[u8; N]> {
    let (first, second) = take_slice(slices, N)?;

    let mut result = [0; N];
    result[..first.len()].copy_from_slice(first);
    result[first.len()..].copy_from_slice(second);

    Some(result)
}

/// Utility function for pulling a variable number of bytes from the front of a
/// pair of slices.
fn take_slice<'a>(
    slices: &mut (&'a [u8], &'a [u8]),
    n: usize,
) -> Option<(&'a [u8], &'a [u8])> {
    let first_n = usize::min(slices.0.len(), n);
    let second_n = n - first_n;
    if slices.1.len() < second_n {
        // Can't pull that many bytes.
        return None;
    }

    let (first, rest1) = slices.0.split_at(first_n);
    slices.0 = rest1;

    let (second, rest2) = slices.1.split_at(second_n);
    slices.1 = rest2;

    Some((first, second))
}

const DATA_LOSS_LEN: usize = 11;
const OVERHEAD: usize = 12;

#[derive(Copy, Clone, Debug)]
enum InsertState {
    /// We successfully recorded the last record (which may have been a loss
    /// record). We will attempt to record the next received record, too.
    Collecting,
    /// We have lost at least one record. We will not record any further records
    /// until we are able to emit a loss record, describing the data loss.
    Losing {
        /// Number of records that have been lost. If this count reaches
        /// `u32::MAX` it saturates.
        count: NonZeroU32,
        /// Timestamp of first lost record.
        timestamp: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    const OUR_FAKE_TID: u16 = 0x1234;
    const ANOTHER_FAKE_TID: u16 = 0x5678;

    #[derive(Copy, Clone, Deserialize, Eq, PartialEq, Debug)]
    struct LossRecord {
        lost: Option<u32>,
    }

    /// Checks generation of the initial "arbitrary data lost" record. Since
    /// many of the tests below discard it before proceeding.
    #[test]
    fn initial_loss_record() {
        let mut s = Store::<64>::DEFAULT;

        s.initialize(OUR_FAKE_TID, 1);

        let initial_contents: Vec<Item<LossRecord>> = copy_contents_as(&mut s);
        let &[r] = initial_contents.as_slice() else {
            panic!("missing initial loss record");
        };
        assert_eq!(r.ena, 1);
        assert_eq!(r.tid, OUR_FAKE_TID);
        assert_eq!(r.timestamp, 1);

        assert_eq!(r.contents.lost, None);

        // Drop it.
        s.flush_thru(r.ena);
        assert!(s.iter_contents().next().is_none());
    }

    /// Passes a record successfully through the queue.
    #[test]
    fn record_thru_queue() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // Insert a thing! We don't care if it's valid CBOR.
        s.insert(ANOTHER_FAKE_TID, 5, b"hello, world!");

        let snapshot = copy_contents_raw(&mut s);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].ena, 2);
        assert_eq!(snapshot[0].tid, ANOTHER_FAKE_TID);
        assert_eq!(snapshot[0].timestamp, 5);
        assert_eq!(snapshot[0].contents, b"hello, world!");

        s.flush_thru(snapshot[0].ena);

        assert_eq!(copy_contents_raw(&mut s), []);
    }

    /// Verifies that a message that consumes 100% of the queue can be enqueued.
    #[test]
    fn filling_completely() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This message just fits.
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 64 - OVERHEAD]);

        assert_eq!(s.free_space(), 0);

        // Which means it should be able to come back out.
        let snapshot = copy_contents_raw(&mut s);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: ANOTHER_FAKE_TID,
                timestamp: 5,
                contents: vec![0; 64 - OVERHEAD]
            }
        );

        // And be flushed.
        s.flush_thru(snapshot[0].ena);
        assert_eq!(s.free_space(), 64);
    }

    /// Tests behavior if the queue goes directly from empty to overflow.
    /// Verifies that the situation is recoverable at the next read (no flush is
    /// required).
    #[test]
    fn data_loss_from_empty_recover_on_read() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This message is juuuuust too long to fit, by one byte.
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 64 - OVERHEAD + 1]);

        // Because the queue is otherwise empty, the next read should produce a
        // data loss message.
        let snapshot: Vec<Item<LossRecord>> = copy_contents_as(&mut s);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: OUR_FAKE_TID,
                timestamp: 5,
                contents: LossRecord { lost: Some(1) },
            }
        );
    }

    /// Tests getting into a "losing" state with valid data in the queue. In
    /// particular, we want to ensure that the loss record is correctly ordered.
    #[test]
    fn data_loss_with_data() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This message fits.
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 28]);
        // This message would fit on its own, but there is not enough room for
        // it.
        s.insert(ANOTHER_FAKE_TID, 10, &[0; 28]);

        // We should still be able to read out the first message, followed by a
        // one-record loss. There should be enough space available in the queue
        // for an immediate recovery without flushing.
        let snapshot = copy_contents_raw(&mut s);
        assert_eq!(snapshot.len(), 2);

        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: ANOTHER_FAKE_TID,
                timestamp: 5,
                contents: vec![0; 28],
            }
        );

        assert_eq!(
            snapshot[1].decode_as::<LossRecord>(),
            Item {
                ena: 3,
                tid: OUR_FAKE_TID,
                timestamp: 10, // time when loss began
                contents: LossRecord { lost: Some(1) },
            }
        );
    }

    /// Tests behavior if multiple records are lost while there is _technically_
    /// enough queue space to allow recovery. We expect the count to increment
    /// rather than generating a sequence of single-record loss events, since
    /// that would make queue pressure even worse.
    #[test]
    fn data_loss_repeated() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This message is juuuuust too long to fit, by one byte.
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 64 - OVERHEAD + 1]);
        // Now that we're in losing state, any message too big to allow recovery
        // just accumulates. Let's do that a few times, shall we?
        for i in 0..10 {
            s.insert(ANOTHER_FAKE_TID, 5 + i, &[0; 64 - OVERHEAD + 1]);
        }

        // Because the queue is otherwise empty, the next read should produce a
        // data loss message.
        let snapshot: Vec<Item<LossRecord>> = copy_contents_as(&mut s);
        assert_eq!(snapshot.len(), 1, "{snapshot:?}");
        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: OUR_FAKE_TID,
                timestamp: 5, // time of _first_ loss
                contents: LossRecord { lost: Some(11) },
            }
        );
    }

    /// Tests that the buffer is never allowed to fill so much that it cannot
    /// fit a loss record. This reproduces a panic where there was insufficient
    /// space to record a loss record.
    #[test]
    fn data_loss_on_full_queue() {
        let mut s = Store::<64>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // Fill half the buffer.
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 32 - OVERHEAD]);
        // Try to fill the other half of the buffer, *to the brim*. Allowing
        // this record in will mean that the buffer no longer has space for a
        // last loss record, so this record should *not* be accepted.
        s.insert(ANOTHER_FAKE_TID, 6, &[0; 32 - OVERHEAD]);
        // This one definitely gets lost.
        s.insert(ANOTHER_FAKE_TID, 7, &[0; 32 - OVERHEAD]);

        let snapshot: Vec<Item<Vec<u8>>> = copy_contents_raw(&mut s);
        assert_eq!(snapshot.len(), 2, "{snapshot:?}");
        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: ANOTHER_FAKE_TID,
                timestamp: 5,
                contents: Vec::from([0; 32 - OVERHEAD])
            }
        );
        assert_eq!(
            snapshot[1].decode_as::<LossRecord>(),
            Item {
                ena: 3,
                tid: OUR_FAKE_TID,
                timestamp: 6,
                contents: LossRecord { lost: Some(2) },
            }
        );
    }

    /// Arranges for the queue to contain: valid data; a loss record; more valid
    /// data. This helps exercise recovery behavior.
    #[test]
    fn data_loss_sandwich() {
        let mut s = Store::<128>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // Insert a message...
        s.insert(ANOTHER_FAKE_TID, 5, &[0; 16]);
        assert_eq!(s.free_space(), 100);
        // Drop a message...
        s.insert(ANOTHER_FAKE_TID, 10, &[0; 100]);
        // Insert a message that will fit along with recovery...
        s.insert(ANOTHER_FAKE_TID, 15, &[0; 16]);

        let snapshot = copy_contents_raw(&mut s);
        assert_eq!(snapshot.len(), 3, "{snapshot:?}");

        assert_eq!(
            snapshot[0],
            Item {
                ena: 2,
                tid: ANOTHER_FAKE_TID,
                timestamp: 5,
                contents: vec![0; 16],
            }
        );

        assert_eq!(
            snapshot[1].decode_as::<LossRecord>(),
            Item {
                ena: 3,
                tid: OUR_FAKE_TID,
                timestamp: 10,
                contents: LossRecord { lost: Some(1) },
            }
        );

        assert_eq!(
            snapshot[2],
            Item {
                ena: 4,
                tid: ANOTHER_FAKE_TID,
                timestamp: 15,
                contents: vec![0; 16],
            }
        );
    }

    /// Tests incremental flushing of a sequence of records.
    #[test]
    fn incremental_flush() {
        let mut s = Store::<128>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // Insert a series of five records occupying ENAs 1-5.
        for i in 0..5 {
            s.insert(ANOTHER_FAKE_TID, 5 + i, &[i as u8]);
        }

        {
            let snapshot = copy_contents_raw(&mut s);
            assert_eq!(snapshot.len(), 5);
            for (i, rec) in snapshot.iter().enumerate() {
                assert_eq!(rec.ena, 2 + i as u64);
                assert_eq!(rec.tid, ANOTHER_FAKE_TID);
                assert_eq!(rec.timestamp, 5 + i as u64);
                assert_eq!(rec.contents, &[i as u8]);
            }

            // Flush just the first record, for the lulz.
            s.flush_thru(snapshot[0].ena);
        }

        {
            // Verify that the tail of 3 records is intact.
            let snapshot = copy_contents_raw(&mut s);
            assert_eq!(snapshot.len(), 4);
            for (i, rec) in snapshot.iter().enumerate() {
                assert_eq!(rec.ena, 3 + i as u64);
                assert_eq!(rec.tid, ANOTHER_FAKE_TID);
                assert_eq!(rec.timestamp, 6 + i as u64);
                assert_eq!(rec.contents, &[i as u8 + 1]);
            }
        }

        // Flush all but the last.
        s.flush_thru(5);
        {
            let snapshot = copy_contents_raw(&mut s);
            assert_eq!(snapshot.len(), 1);
            for (i, rec) in snapshot.iter().enumerate() {
                assert_eq!(rec.ena, 6 + i as u64);
                assert_eq!(rec.tid, ANOTHER_FAKE_TID);
                assert_eq!(rec.timestamp, 9 + i as u64);
                assert_eq!(rec.contents, &[i as u8 + 4]);
            }
        }
        // Finally...
        s.flush_thru(6);
        assert_eq!(copy_contents_raw(&mut s), []);
    }

    /// Tests flushing records that are already gone.
    #[test]
    fn old_enas_are_nops() {
        let mut s = Store::<128>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This record occupies ENA 1
        s.insert(ANOTHER_FAKE_TID, 5, &[1]);
        // ENA 2
        s.insert(ANOTHER_FAKE_TID, 6, &[2]);

        assert_eq!(s.stored_record_count, 2);

        // Flushing ENA 1 should have no effect (we already got rid of it)
        s.flush_thru(1);
        assert_eq!(s.stored_record_count, 2);

        // Flushing ENA 2 should drop one record.
        s.flush_thru(2);
        assert_eq!(s.stored_record_count, 1);

        // 0 and 1 are both no-ops now.
        for ena in [0, 1, 2] {
            s.flush_thru(ena);
            assert_eq!(s.stored_record_count, 1);
        }
    }

    /// Ensures that we don't throw away the contents of the queue if someone
    /// sends us a silly large number.
    #[test]
    fn future_enas_are_nops() {
        let mut s = Store::<128>::DEFAULT;
        s.initialize(OUR_FAKE_TID, 1);
        consume_initial_loss(&mut s);

        // This record occupies ENA 1
        s.insert(ANOTHER_FAKE_TID, 5, &[1]);
        // ENA 2
        s.insert(ANOTHER_FAKE_TID, 6, &[2]);

        assert_eq!(s.stored_record_count, 2);

        // ENA 4 has not yet been issued, and should not cause any change:
        s.flush_thru(4);
        assert_eq!(s.stored_record_count, 2);
    }

    fn consume_initial_loss<const N: usize>(s: &mut Store<N>) {
        let initial_contents: Vec<Item<LossRecord>> = copy_contents_as(s);
        let &[r] = initial_contents.as_slice() else {
            panic!("missing initial loss record");
        };
        assert_eq!(r.ena, 1);
        assert_eq!(r.tid, OUR_FAKE_TID);
        assert_eq!(r.timestamp, 1);

        assert_eq!(r.contents.lost, None);

        // Drop it.
        s.flush_thru(r.ena);
    }

    fn copy_contents_raw<const N: usize>(
        s: &mut Store<N>,
    ) -> Vec<Item<Vec<u8>>> {
        s.iter_contents()
            .map(|r| {
                let bytes = r.body_bytes().collect::<Vec<_>>();
                Item {
                    ena: r.ena,
                    tid: r.tid,
                    timestamp: r.timestamp,
                    contents: bytes,
                }
            })
            .collect::<Vec<_>>()
    }

    fn copy_contents_as<'a, T, const N: usize>(
        s: &'a mut Store<N>,
    ) -> Vec<Item<T>>
    where
        T: for<'d> Deserialize<'d>,
    {
        s.iter_contents()
            .map(|r| {
                let bytes = r.body_bytes().collect::<Vec<_>>();
                let contents: T = minicbor_serde::from_slice(&bytes).unwrap();
                Item {
                    ena: r.ena,
                    tid: r.tid,
                    timestamp: r.timestamp,
                    contents,
                }
            })
            .collect::<Vec<_>>()
    }

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct Item<T> {
        ena: u64,
        tid: u16,
        timestamp: u64,
        contents: T,
    }

    impl Item<Vec<u8>> {
        fn decode_as<T>(&self) -> Item<T>
        where
            T: for<'a> Deserialize<'a>,
        {
            Item {
                ena: self.ena,
                tid: self.tid,
                timestamp: self.timestamp,
                contents: minicbor_serde::from_slice(&self.contents).unwrap(),
            }
        }
    }
}
