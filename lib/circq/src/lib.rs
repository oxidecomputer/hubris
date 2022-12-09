#![cfg_attr(not(test), no_std)]

/// A simple circular byte queue, backed by borrowed memory.
///
/// When given an `n`-byte slice of backing memory, a `CircQ` can store up to
/// `n` bytes in FIFO order. Bytes can be enqueued or dequeued in chunks; the
/// size of these chunks are not recorded in the queue, so the enqueue and
/// dequeue sizes don't have to match.
///
/// This is intended for storing variable-length messages, which will require
/// some sort of message terminator or explicit length stored in the queue.
/// (Otherwise there's no good way for you to find the length of a message
/// you're reading out.)
///
/// # Design goals
///
/// There are many ways of implementing a circular buffer. This version's design
/// goals are:
///
/// 1. `no_std`
/// 2. Use borrowed memory, so that a queue can be backed by a named `static`
///    for debug visibility, rather than some random location on the stack.
/// 3. Allow efficient enqueue/dequeue of blocks of bytes using slice copies,
///    and _in particular_ the option of bringing-your-own slice copy mechanism,
///    so that it can be efficiently used with the borrow syscalls in Hubris.
/// 4. Code clarity -- there are many arithmetical tricks in circular queue
///    implementation, and this uses none of them.
///
/// Non-goals:
///
/// - Concurrent access or sharing. The queue must always be accessed using
///   `&mut`. It is `Send` but not `Sync`.
/// - Being the most efficient queue ever.
#[derive(Debug)]
pub struct CircQ<'s> {
    backing: &'s mut [u8],
    head: usize,
    tail: usize,
    available: usize,
}

/// Error returned when the queue is too full to accommodate a block.
#[derive(Copy, Clone, Debug)]
pub struct QueueFull;

/// Error returned when the queue doesn't have enough data to read out a certain
/// number of bytes.
#[derive(Copy, Clone, Debug)]
pub struct QueueNotFullEnough;

impl<'s> CircQ<'s> {
    /// Creates a queue structure with the given backing memory. The queue is
    /// initially empty.
    pub fn new(backing: &'s mut [u8]) -> Self {
        Self {
            backing,
            head: 0,
            tail: 0,
            available: 0,
        }
    }

    /// Checks whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// Returns the number of bytes that have been enqueued but not yet
    /// dequeued.
    pub fn available(&self) -> usize {
        self.available
    }

    /// Returns the number of bytes that can be enqueued without needing to
    /// dequeue more data.
    pub fn free(&self) -> usize {
        self.backing.len() - self.available()
    }

    /// Enqueues a single byte (convenience function).
    pub fn enqueue1(&mut self, byte: u8) -> Result<(), QueueFull> {
        let (first, _second) = self.enqueue_space(1)?;
        first[0] = byte;
        Ok(())
    }

    /// Enqueues a block of bytes using slice copies.
    ///
    /// This is a convenience wrapper around `enqueue_space` that keeps you from
    /// having to think about discontiguous slices.
    pub fn enqueue(&mut self, data: &[u8]) -> Result<(), QueueFull> {
        let (first, second) = self.enqueue_space(data.len())?;
        let (d1, d2) = data.split_at(first.len());
        first.copy_from_slice(d1);
        second.copy_from_slice(d2);
        Ok(())
    }

    /// Enqueue a new block of `n` bytes but _do not write it yet._
    ///
    /// If space is available, returns two mutable slice references, whose
    /// lengths will _sum_ to `n`; the second one may be empty. The data you are
    /// enqueueing should be written to the first slice, and then the second, in
    /// that order. Here are two examples.
    ///
    /// First: if the current head position of the queue is `n` bytes or more
    /// away from the end of backing memory, then your request can be handled
    /// using contiguous RAM, yay:
    ///
    /// ```text
    ///
    ///    0                       S
    ///    +-----------------------+
    ///    |       memory          |
    ///    +-----------------------+
    ///           ^-----------^
    ///           head        head+n
    /// ```
    ///
    /// In this case, the first slice returned will be `n` bytes long, and the
    /// second will be empty.
    ///
    /// Second: if this is _not_ true, your request wraps around the backing
    /// memory, and the data you're enqueuing will go into two discontiguous
    /// areas:
    ///
    /// ```text
    ///
    ///    0                       S
    ///    +-----------------------+
    ///    |       memory          |
    ///    +-----------------------+
    ///    --^                ^-----
    ///      (head+n) % S     head  
    ///    \_/                \____/
    ///     second            first
    /// ```
    ///
    /// In this case, the _first_ slice returned will start at the head pointer
    /// and continue to the end of backing memory, and the _second_ will start
    /// at the base of backing memory.
    ///
    /// This API is designed so that you, the client, don't have to think about
    /// these two cases. Instead, write your data to the two slices in turn.
    /// Generic code for doing this (assuming your data is in memory, so we can
    /// use slice copies) looks like this:
    ///
    /// ```rust
    /// # use circq::*;
    /// # fn main() -> Result<(), QueueFull> {
    /// # let mut backing = [0; 20];
    /// # let mut q = CircQ::new(&mut backing);
    ///
    /// let my_data = b"hello, world";
    ///
    /// let (first, second) = q.enqueue_space(my_data.len())?;
    /// let (data1, data2) = my_data.split_at(first.len());
    /// first.copy_from_slice(data1);
    /// second.copy_from_slice(data2);
    ///
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// (Note that if you find yourself doing _exactly that,_ the
    /// [`Self::enqueue`] function has you covered.)
    ///
    /// You are expected to initialize the contents of the two slices. If you
    /// fail to do that, the queue will contain `n` bytes of some arbitrary data
    /// (likely earlier messages).
    ///
    /// Once `enqueue_space` has returned, the queue head pointer has been
    /// bumped to contain `n` more bytes; there is no way to "undo" this.
    pub fn enqueue_space(&mut self, n: usize) -> Result<(&mut [u8], &mut [u8]), QueueFull> {
        if n > self.free() {
            return Err(QueueFull);
        }

        let backlen = self.backing.len();

        let result = region_mut(self.backing, n, self.head, self.tail);
        self.head = circular_add(self.head, n, backlen);
        self.available += n;

        Ok(result)
    }

    /// Dequeue one byte from the queue (convenience function).
    pub fn dequeue1(&mut self) -> Result<u8, QueueNotFullEnough> {
        let (first, _) = self.dequeue_space(1)?;
        Ok(first[0])
    }

    /// Dequeue `dest.len()` bytes and copy them into `dest` using slice copies.
    ///
    /// This is a convenience wrapper around `dequeue_space` that keeps you from
    /// having to think about discontiguous slices.
    pub fn dequeue_into(&mut self, dest: &mut [u8]) -> Result<(), QueueNotFullEnough> {
        let (first, second) = self.dequeue_space(dest.len())?;
        let (d1, d2) = dest.split_at_mut(first.len());
        d1.copy_from_slice(first);
        d2.copy_from_slice(second);
        Ok(())
    }

    /// Takes `n` bytes from the queue and returns a reference to them as a pair
    /// of slices, to support cases where the `n` bytes are not contiguous in
    /// memory. The lengths of the two returned slices sum to `n`.
    ///
    /// If your data is going into RAM and you're just going to copy it out of
    /// the queue, see [`Self::dequeue`], which does just that.
    ///
    /// The behavior of this function is _almost identical_ to
    /// [`Self::enqueue_space`], except that data is taken starting at the tail
    /// pointer instead of the head pointer, and you're expected to read rather
    /// than write the data (though you may write it if you wish, it's no longer
    /// being used). See [`Self::enqueue_space`] for nice diagrams.
    ///
    /// Copy or interpret the data referenced by the slices and then drop them
    /// -- the compiler will ensure that you drop them before doing anything
    /// else to the queue.
    pub fn dequeue_space(&mut self, n: usize) -> Result<(&mut [u8], &mut [u8]), QueueNotFullEnough> {
        if n > self.available() {
            return Err(QueueNotFullEnough);
        }

        let backlen = self.backing.len();

        let result = region_mut(self.backing, n, self.tail, self.head);

        self.tail = circular_add(self.tail, n, backlen);
        self.available -= n;

        Ok(result)
    }
}

/// Implementation factor of enqueue/dequeue.
///
/// Given `backing` memory, find a possibly discontiguous section of `n` bytes
/// starting at `from` and _not crossing_ `to`.
///
/// If there aren't `n` bytes between `from` and `to` (modulo `backing.len()`)
/// this will panic.
fn region_mut(backing: &mut [u8], n: usize, from: usize, to: usize) -> (&mut [u8], &mut [u8]) {
    if from < to {
        // Our entire region can be contiguous.
        debug_assert!(to - from >= n);
        (&mut backing[from..from + n], &mut [])
    } else {
        // We may need to contiguous regions.
        // Compute the size of contiguous region available starting at
        // 'from'.
        let first_len = (backing.len() - from).min(n);
        let second_len = n.saturating_sub(first_len);
        debug_assert!(second_len <= to);

        // Split the backing reference.
        let (second_plus, first_plus) = backing.split_at_mut(from);
        // Truncate both regions as necessary.
        let first = &mut first_plus[..first_len];
        let second = &mut second_plus[..second_len];
        (first, second)
    }
}

/// Common code for doing circular arithmetic without assuming hardware divide.
fn circular_add(a: usize, b: usize, limit: usize) -> usize {
    let n = a + b;
    // This slightly weird formulation avoids generating an overflow panic that
    // the compiler would have to optimize away.
    n.checked_sub(limit).unwrap_or(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_state() {
        let mut backing = [0; 16];
        let q = CircQ::new(&mut backing);

        assert!(q.is_empty());
        assert_eq!(q.available(), 0);
    }

    #[test]
    fn enqueue_0() {
        let mut backing = [0; 16];
        let mut q = CircQ::new(&mut backing);

        for i in 0..16 {
            let (first, second) = q.enqueue_space(0).expect(&format!("enqueueing zero bytes after {i} bytes should succeed"));
            assert!(first.is_empty());
            assert!(second.is_empty());
            q.enqueue_space(1).unwrap();
        }
        // Note that this checks that enqueueing zero bytes succeeds _even when
        // the queue is full,_ which is a weird behavior but is internally
        // consistent.
        let (first, second) = q.enqueue_space(0).expect("enqueueing zero bytes on a full q should succeed");
        assert!(first.is_empty());
        assert!(second.is_empty());
    }

    #[test]
    fn enqueue_string() {
        let mut backing = [0; 16];
        let mut q = CircQ::new(&mut backing);

        let test_string = b"ABCDEFGHIJKLMNOP";
        assert_eq!(test_string.len(), 16); // Don't break this plz

        for (i, &byte) in test_string.iter().enumerate() {
            assert_eq!(q.available(), i);
            assert_eq!(q.free(), 16 - i);

            q.enqueue1(byte).expect(&format!("enqueueing one byte after {i} bytes should succeed"));
        }
        assert_eq!(q.available(), 16);
        assert_eq!(q.free(), 0);

        // This should fail once the queue is full.
        if q.enqueue1(0).is_ok() {
            panic!("should not be able to enqueue another byte after queue is full");
        }

        for (i, &expected_byte) in test_string.iter().enumerate() {
            let b = q.dequeue1().expect(&format!("enqueueing one byte after {i} bytes should succeed"));
            assert_eq!(b, expected_byte);
        }

        // This should fail once the queue is empty.
        if q.dequeue1().is_ok() {
            panic!("should not be able to dequeue1 from empty queue");
        }
    }

    #[test]
    fn enqueue_discontiguous_full_extent() {
        let mut backing = [0; 16];

        for i in 0..16 {
            let mut q = CircQ::new(&mut backing);
            // Shift the q head/tail to i
            q.enqueue_space(i).unwrap();
            q.dequeue_space(i).unwrap();

            // Now attempt to fill the queue.
            if let Ok((first, second)) = q.enqueue_space(16) {
                assert_eq!(first.len(), 16 - i );
                assert_eq!(second.len(), i);
                for (i, byte) in first.iter_mut().chain(second).enumerate() {
                    *byte = i as u8;
                }
            } else {
                panic!("can't fill queue from offset {i}");
            }

            // And see if we can read it back.
            if let Ok((first, second)) = q.dequeue_space(16) {
                assert_eq!(first.len() + second.len(), 16);
                for (i, byte) in first.iter().chain(&*second).enumerate() {
                    assert_eq!(*byte, i as u8);
                }
            } else {
                panic!("can't read back entirety of queue at offset {i}");
            }
        }
    }
}
