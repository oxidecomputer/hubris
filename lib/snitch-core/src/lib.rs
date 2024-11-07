//! Portable core data structures for the `snitch` error reporting task.
//!
//! This is factored out to make it easier to unit test.
//!
//! # Why are you like this
//!
//! The purpose of the `snitch` is to record failures for future scrutiny. This
//! is the most annoying possible time to lose data, so its backing queue
//! (`Store` in this crate) is quite specialized. It's trying to do two things:
//!
//! 1. Minimize the occasions when data is lost due to queue space limitations.
//! 2. When it nevertheless happens, record it as part of the sequence.
//!
//! ## Logical queue behavior
//!
//! Logically, the queue contains an arbitrary interleaving of two types of
//! records:
//!
//! - Messages, which are uninterpreted strings of up to 65535 bytes each.
//! - Inline loss records, which indicate that at a particular point in the
//!   sequence, `n` records were received but could not fit in the queue.
//!
//! (The limit to 65535 bytes is an internal implementation detail that is
//! intended to simplify the code. It could be lifted.)
//!
//! Loss records are tracked inline, rather than with a global counter or
//! something, so that engineers staring at a sequence of error messages won't
//! be misled into thinking two events happened back-to-back when they didn't.
//! The one exception to this is if we get a string of `2**32-1` losses in a
//! row, at which point the counter saturates and just starts meaning "a bunch."
//!
//! ## In-memory representation
//!
//! Because failures tend to happen in clusters, it's important to maximize the
//! space efficiency of the queue --- we expect it to go from empty to nearly
//! full in short order, before being drained back down. To this end, the queue
//! is a circular buffer of bytes with no internal alignment or padding.
//!
//! Currently, the in-memory representation of the queue contents is a sequence
//! of _frames,_ where each frame starts with an identifying byte: `1` for an
//! error message, `0xFF` for a loss record. This is followed by a type-specific
//! payload:
//!
//! - Message: a length as a little-endian unaligned `u16`, followed by that
//! many bytes. (So the overall length is the recorded length plus 3.)
//!
//! - Loss record: a little-endian unaligned `u32`. The value `0` is reserved to
//!   mean "too many to count." This punning of zero is hidden at the API layer.
//!
//! ## About the weird API
//!
//! In practice, `snitch` will copy messages in and out by using IPC borrows.
//! Copying to or from a borrow can fail, in which case we don't want to throw
//! valuable data away.
//!
//! As a result, all the queue APIs are _fallible._ They give the caller a way
//! to cancel enqueue/dequeue before it completes. This makes things a little
//! more awkward than you might expect.

use std::{convert::Infallible, mem::take, num::NonZeroU32};

use unwrap_lite::UnwrapLite as _;

/// It's often convenient to be able to materialize a NonZeroU32 with the value
/// 1, without using code that might panic. NonZeroU32 happens to contain a
/// const defined as 1, but its name is strange (`MIN`). Let's give it a better
/// name.
const ONE: NonZeroU32 = NonZeroU32::MIN;

const MESSAGE: u8 = 1;
const LOSS: u8 = 0xFF;
const LOSS_LEN: usize = 5;

#[derive(Debug)]
pub struct Store<const N: usize> {
    /// Backing store.
    contents: [u8; N],
    /// Index within `contents` where the next byte will be written.
    next_write: usize,
    /// Index within `contents` where the next byte will be read.
    next_read: usize,
    /// When `next_write == next_read`, this flag distinguishes the full from
    /// empty condition.
    full: bool,
    /// Tracks whether the writer is losing data.
    writer_state: WriterState,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WriterState {
    /// All data has been written to the queue. The writer should attempt to
    /// write new data.
    Writing,
    /// Data has been lost. The writer should attempt to recover but may wind up
    /// just incrementing this number. If the number is `None` it has overflowed
    /// and is "too many to count."
    Losing(Option<NonZeroU32>),
}

impl<const N: usize> Store<N> {
    pub const DEFAULT: Self = Self {
        contents: [0; N],
        next_write: 0,
        next_read: 0,
        full: false,
        writer_state: WriterState::Writing,
    };

    /// Convenience version of `enqueue_record` for when the data is already
    /// contiguous in RAM.
    pub fn enqueue_record_slice(&mut self, slice: &[u8]) -> Result<(), StoreError<Infallible>> {
        self.enqueue_record(slice.len(), |d0, d1| {
            let (s0, s1) = slice.split_at(d0.len());
            d0.copy_from_slice(s0);
            d1.copy_from_slice(s1);
            Ok(())
        })
    }

    pub fn enqueue_record<E>(&mut self, len: usize, copy_in: impl FnOnce(&mut [u8], &mut [u8]) -> Result<(), E>) -> Result<(), StoreError<E>> {
        let f = self.bytes_free();

        if let WriterState::Losing(lost_count) = &mut self.writer_state {
            // We are in a losing-data situation. However, if we entered this
            // state due to a message being really big, and this message is much
            // smaller, it's possible that we can escape the state even without
            // a reader freeing any space in the queue.
            //
            // Let's check.
            if f >= LOSS_LEN + required_space_for(len) {
                // We can record the loss _and_ this message! Start with the
                // loss.
                let n = *lost_count;
                self.record_loss(n);
                self.writer_state = WriterState::Writing;
                // Fall through to normal handling code below.
            } else {
                // Still stuck.
                if let Some(n) = lost_count {
                    // Set to `None` on overflow.
                    *lost_count = n.checked_add(1);
                } else {
                    // Count is already saturated and will remain so.
                }
                return Err(StoreError::NotEnoughSpace);
            }
        }

        match self.enqueue_record_without_tracking(len, copy_in) {
            Ok(()) => Ok(()),

            Err(StoreError::NotEnoughSpace) => {
                // If we got here, it means we are _newly_ losing data. Enter
                // the loss state.
                self.writer_state = WriterState::Losing(Some(ONE));
                Err(StoreError::NotEnoughSpace)
            }

            Err(StoreError::CopyError(e)) => {
                // This isn't our fault, and we haven't committed the space.
                // It's not even clear that this represents a lost message. Punt
                // to the caller.
                Err(StoreError::CopyError(e))
            }
        }
    }

    fn enqueue_record_without_tracking<E>(&mut self, len: usize, copy_in: impl FnOnce(&mut [u8], &mut [u8]) -> Result<(), E>) -> Result<(), StoreError<E>> {
        // See if we have enough room to enqueue this message.
        let required = required_space_for(len);
        let mut slices = self.prepare_write(required).ok_or(StoreError::NotEnoughSpace)?;

        // Write the "valid message" header.
        let [len16lo, len16hi] = u16::try_from(len).map_err(|_| StoreError::NotEnoughSpace)?.to_le_bytes();
        push_to_slices(&mut slices, MESSAGE);
        push_to_slices(&mut slices, len16lo);
        push_to_slices(&mut slices, len16hi);

        copy_in(slices.0, slices.1).map_err(StoreError::CopyError)?;

        self.finish_write(required);
        Ok(())
    }

    /// Gets the next `n` writable bytes as a pair of slices, the lengths of
    /// which sum to `n`.
    ///
    /// If there aren't `n` free writable bytes, returns `None`.
    ///
    /// Once a message has been copied in using this operation, use
    /// `finish_write` to mark it as used.
    fn prepare_write(&mut self, n: usize) -> Option<(&mut [u8], &mut [u8])> {
        if n > self.bytes_free() {
            return None;
        }

        let (second, first) = self.contents.split_at_mut(self.next_write);
        let n1 = usize::min(n, first.len());
        let n2 = n.saturating_sub(n1);
        Some((&mut first[..n1], &mut second[..n2]))
    }

    fn finish_write(&mut self, n: usize) {
        debug_assert!(n <= self.bytes_free());
        self.next_write = (self.next_write + n) % N;
        if self.next_write == self.next_read {
            self.full = true;
        }
    }

    fn record_loss(&mut self, count: Option<NonZeroU32>) {
        // We use the special value 0 to represent "overflow."
        let count = count.map(NonZeroU32::get).unwrap_or(0);

        let mut slices = self.prepare_write(LOSS_LEN).unwrap_lite();
        push_to_slices(&mut slices, LOSS);
        copy2(&count.to_le_bytes(), slices.0, slices.1);

        self.finish_write(LOSS_LEN);
    }

    pub fn dequeue_record<E>(&mut self, copy_out: impl FnOnce(Record<'_>) -> Result<(), E>) -> Result<(), ReadError<E>> {
        match self.dequeue_record_without_discard(copy_out) {
            Ok(()) => Ok(()),
            Err(ReadError::InternalCorruption) => {
                // We're going to tell the caller we're corrupt, but we're also
                // going to clear the condition that caused it so that
                // operation can resume with limited data loss.
                self.next_read = self.next_write;
                self.full = false;
                Err(ReadError::InternalCorruption)
            }
            Err(ReadError::Empty) => Err(ReadError::Empty),
            Err(ReadError::CopyError(e)) => Err(ReadError::CopyError(e)),
        }
    }

    fn dequeue_record_without_discard<E>(&mut self, copy_out: impl FnOnce(Record<'_>) -> Result<(), E>) -> Result<(), ReadError<E>> {
        if self.next_read == self.next_write && !self.full {
            // We may have stored losses that we can read out, thereby freeing
            // the writer to resume.
            if let WriterState::Losing(n) = self.writer_state {
                copy_out(Record::Lost(n)).map_err(ReadError::CopyError)?;
                self.writer_state = WriterState::Writing;
                return Ok(());
            } else {
                // There's nothing to dequeue.
                return Err(ReadError::Empty);
            }
        }

        // we reuse this pattern a few times below:
        fn check<E>(condition: bool) -> Result<(), ReadError<E>> {
            if !condition {
                Err(ReadError::InternalCorruption)
            } else {
                Ok(())
            }
        }

        match self.contents[self.next_read] {
            self::MESSAGE => {
                // Variable length data message.
                let avail = self.bytes_avail();
                check(avail >= 3)?;
                let lo = self.contents[(self.next_read + 1) % N];
                let hi = self.contents[(self.next_read + 2) % N];
                let len = usize::from(u16::from_le_bytes([lo, hi]));
                check(len <= avail - 3)?;

                let start = (self.next_read + 3) % N;
                let (slice1, slice0) = self.contents.split_at(start);
                let len0 = usize::min(len, slice0.len());
                let len1 = len.saturating_sub(len0);
                let (slice0, slice1) = (&slice0[..len0], &slice1[..len1]);

                copy_out(Record::Valid(slice0, slice1)).map_err(ReadError::CopyError)?;

                self.next_read = (start + len) % N;
                self.full = false;
                Ok(())
            }
            self::LOSS => {
                // Inline loss record.
                let avail = self.bytes_avail();
                check(avail >= LOSS_LEN)?;
                let bytes = [
                    self.contents[(self.next_read + 1) % N],
                    self.contents[(self.next_read + 2) % N],
                    self.contents[(self.next_read + 3) % N],
                    self.contents[(self.next_read + 4) % N],
                ];
                let lost = u32::from_le_bytes(bytes);
                let lost = NonZeroU32::try_from(lost).ok(); // zero => None

                copy_out(Record::Lost(lost)).map_err(ReadError::CopyError)?;

                self.next_read = (self.next_read + LOSS_LEN) % N;
                self.full = false;
                Ok(())
            }
            _uhhhh => {
                // Well, great, the queue is corrupt. We indicate this condition
                // to the caller instead of panicking so it can be reported more
                // easily upstream. (Note that this does not _clear_ the
                // corruption; the next-outer layer does that.)
                Err(ReadError::InternalCorruption)
            }
        }
    }

    fn bytes_free(&self) -> usize {
        N - self.bytes_avail()
    }

    fn bytes_avail(&self) -> usize {
        if self.full {
            N
        } else if self.next_write == self.next_read {
            0
        } else if self.next_write < self.next_read {
            N - (self.next_read - self.next_write)
        } else {
            self.next_write - self.next_read
        }
    }
}

fn push_to_slices(slices: &mut (&mut [u8], &mut [u8]), byte: u8) {
    if slices.0.is_empty() {
        slices.1[0] = byte;
        let s = take(&mut slices.1);
        slices.1 = &mut s[1..];
    } else {
        slices.0[0] = byte;
        let s = take(&mut slices.0);
        slices.0 = &mut s[1..];
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoreError<E> {
    NotEnoughSpace,
    CopyError(E),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReadError<E> {
    Empty,
    InternalCorruption,
    CopyError(E),
}

#[derive(Copy, Clone, Debug)]
pub enum Record<'a> {
    Valid(&'a [u8], &'a [u8]),
    Lost(Option<NonZeroU32>),
}

const fn required_space_for(message_len: usize) -> usize {
    // A valid message has 3 bytes of overhead: marker, and two bytes of length.
    // Note that this is an overflowing addition.
    message_len + 3
}

fn copy2(src: &[u8], dest0: &mut [u8], dest1: &mut [u8]) {
    assert_eq!(src.len(), dest0.len() + dest1.len());
    dest0.copy_from_slice(&src[..dest0.len()]);
    dest1.copy_from_slice(&src[dest0.len()..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test case exists mostly to spot over/underflows in index handling
    /// for zero-sized queues.
    #[test]
    fn zero_sized_queue_wont_enqueue() {
        let mut q = Store::<0>::DEFAULT;
        assert_eq!(q.bytes_free(), 0);
        assert_eq!(q.bytes_avail(), 0);

        // Try the empty message, plus lengths up through the size of the
        // default message header, just in case there's header-related bogus
        // math.
        for len in 0..4 {
            assert_eq!(q.enqueue_record(len, |_, _| Err(())), Err(StoreError::NotEnoughSpace),
                "len is {len}");
            assert_eq!(q.bytes_avail(), 0);
        }
    }

    /// This test case exists mostly to spot over/underflows in index handling
    /// for zero-sized queues.
    #[test]
    fn zero_sized_queue_wont_dequeue() {
        let mut q = Store::<0>::DEFAULT;

        assert_eq!(q.dequeue_record(|_| Err(())), Err(ReadError::Empty));
    }

    #[test]
    fn nonzero_bytes_free() {
        let q = Store::<10>::DEFAULT;
        assert_eq!(q.bytes_free(), 10);
        assert_eq!(q.bytes_avail(), 0);
    }

    /// Queues like this often have off-by-one issues when they become
    /// completely full, so, let's check it.
    #[test]
    fn enqueue_can_fill_queue() {
        const MESSAGE: &[u8] = b"omglol";
        const SIZE: usize = required_space_for(MESSAGE.len());

        let mut q = Store::<SIZE>::DEFAULT;

        q.enqueue_record_slice(MESSAGE)
            .expect("enqueue should succeed");

        assert_eq!(q.bytes_avail(), SIZE);
        assert_eq!(q.bytes_free(), 0);

        q.dequeue_record::<()>(|rec| if let Record::Valid(s0, s1) = rec {
            assert_eq!(s0, &MESSAGE[..s0.len()]);
            assert_eq!(s1, &MESSAGE[s0.len()..]);
            Ok(())
        } else {
            panic!("expected record, got: {rec:?}");
        }).expect("should dequeue");
    }

    #[test]
    fn enqueue_dequeue_several() {
        const SIZE: usize = 40;
        let mut q = Store::<SIZE>::DEFAULT;

        // Offset the pointers so we're not starting out with things at zero.
        // Zero is the easy case. Wraparound is more interesting.
        q.next_read = 5;
        q.next_write = 5;
        // 35 bytes until wraparound
        
        // 8 byte message => 11 bytes used => 29 bytes free
        const MESSAGE1: &[u8] = b"12345678";
        q.enqueue_record_slice(MESSAGE1).expect("should fit");
        assert_eq!(q.bytes_avail(), 8 + 3);
        assert_eq!(q.bytes_free(), SIZE - (8 + 3));
        // 4 byte message => 18 bytes used => 22 bytes free
        const MESSAGE2: &[u8] = b"1234";
        q.enqueue_record_slice(MESSAGE2).expect("should fit");

        assert_eq!(q.bytes_free(), 22);

        // Message should dequeue in order as valid.
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE1[..s0.len()]);
            assert_eq!(s1, &MESSAGE1[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");

        // We're back up to 33 bytes free.

        // Force wraparound.
        const MESSAGE3: [u8; 25] = [0; 25];
        // 25 byte message => 28 bytes written => 5 bytes free
        q.enqueue_record_slice(&MESSAGE3).expect("should fit");
        assert_eq!(q.bytes_free(), 5, "{q:?}");

        // Test dequeueing around the end of the array.
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE2[..s0.len()]);
            assert_eq!(s1, &MESSAGE2[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE3[..s0.len()]);
            assert_eq!(s1, &MESSAGE3[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
    }

    /// A _recoverable loss_ is a failure to record some data, but without
    /// leaving the queue unable to store additional messages.
    #[test]
    fn enqueue_recoverable_loss() {
        const SIZE: usize = 15;
        let mut q = Store::<SIZE>::DEFAULT;

        // 2 byte message + 3 bytes overhead = 5 bytes used, 10 bytes free.
        const MESSAGE1: [u8; 2] = [0; 2];
        q.enqueue_record_slice(&MESSAGE1).expect("should fit");
        // 8 byte message + 3 bytes overhead will not fit now. This kicks us
        // into "losing" state, but does not consume any storage space.
        assert_eq!(
            q.enqueue_record_slice(&[0; 8]),
            Err(StoreError::NotEnoughSpace),
        );
        // We can, however, fit a 2-byte message. This will consume:
        // - 5 bytes for the inline loss record
        // - then 3 bytes overhead + 2 bytes payload
        // ...leaving the queue full.
        const MESSAGE2: [u8; 2] = [0; 2];
        q.enqueue_record_slice(&MESSAGE2).expect("should fit");
        
        // Reading things out of the queue preserves the relative ordering of
        // the data loss now.
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE1[..s0.len()]);
            assert_eq!(s1, &MESSAGE1[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
        q.dequeue_record(|r| if let Record::Lost(n) = r {
            assert_eq!(n, NonZeroU32::new(1));
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue a loss");
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE2[..s0.len()]);
            assert_eq!(s1, &MESSAGE2[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
    }

    /// We can record `2**32-1` lost messages before saturating, without
    /// requiring queue space. This verifies that (well, part way).
    #[test]
    fn enqueue_repeated_loss() {
        const SIZE: usize = 15;
        let mut q = Store::<SIZE>::DEFAULT;

        // 2 byte message + 3 bytes overhead = 5 bytes used, 10 bytes free.
        const MESSAGE1: [u8; 2] = [0; 2];
        q.enqueue_record_slice(&MESSAGE1).expect("should fit");

        // 8 byte message + 3 bytes overhead will not fit now. This kicks us
        // into "losing" state, but does not consume any storage space.
        for _ in 0..100 {
            assert_eq!(
                q.enqueue_record_slice(&[0; 8]),
                Err(StoreError::NotEnoughSpace),
            );
        }

        // We can, however, fit a 2-byte message. This will consume:
        // - 5 bytes for the inline loss record
        // - then 3 bytes overhead + 2 bytes payload
        // ...leaving the queue full.
        const MESSAGE2: [u8; 2] = [0; 2];
        q.enqueue_record_slice(&MESSAGE2).expect("should fit");
        
        // Reading things out of the queue preserves the relative ordering of
        // the data loss now.
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE1[..s0.len()]);
            assert_eq!(s1, &MESSAGE1[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
        q.dequeue_record(|r| if let Record::Lost(n) = r {
            assert_eq!(n, NonZeroU32::new(100));
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue a loss");
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE2[..s0.len()]);
            assert_eq!(s1, &MESSAGE2[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
    }

    /// The recoverable loss tests demonstrate cases where a writer can free
    /// _itself_ from a losing-data state; this verifies that a reader can also
    /// cause this.
    #[test]
    fn enqueue_loss_freed_by_read() {
        const SIZE: usize = 15;
        let mut q = Store::<SIZE>::DEFAULT;

        // 2 byte message + 3 bytes overhead = 5 bytes used, 10 bytes free.
        const MESSAGE1: [u8; 2] = [0; 2];
        q.enqueue_record_slice(&MESSAGE1).expect("should fit");
        // 8 byte message + 3 bytes overhead will not fit now. This kicks us
        // into "losing" state, but does not consume any storage space.
        for _ in 0..100 {
            assert_eq!(
                q.enqueue_record_slice(&[0; 8]),
                Err(StoreError::NotEnoughSpace),
            );
        }

        // Drain the queue to free the writer.
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE1[..s0.len()]);
            assert_eq!(s1, &MESSAGE1[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
        q.dequeue_record(|r| if let Record::Lost(n) = r {
            assert_eq!(n, NonZeroU32::new(100));
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue a loss");

        // Should be ok to enqueue more now:
        q.enqueue_record_slice(&MESSAGE1).expect("should fit");
        // and get it back out
        q.dequeue_record(|r| if let Record::Valid(s0, s1) = r {
            assert_eq!(s0, &MESSAGE1[..s0.len()]);
            assert_eq!(s1, &MESSAGE1[s0.len()..]);
            Ok(())
        } else {
            Err(())
        }).expect("should dequeue");
    }

}
