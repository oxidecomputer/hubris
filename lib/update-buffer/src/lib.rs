// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An extremely simple buffer type intended for use where we want to create a
//! single static buffer but share its use among different clients.
//!
//! For example, if we have a task that wants to accept updates that are written
//! in chunk sizes of 256 and 1024 bytes, we can declare a single, static
//! [`UpdateBuffer<1024>`], and then borrow [`BorrowedUpdateBuffer`]s of sizes
//! 256 and/or 1024 when needed (but not both simultaneously).

#![cfg_attr(not(test), no_std)]

use core::{mem, ops::Deref};
use spin::Mutex;
use spin::MutexGuard;

#[derive(Debug)]
pub struct UpdateBuffer<T, const N: usize> {
    current_owner: Mutex<Option<T>>,
    data: Mutex<[u8; N]>,
}

impl<T, const N: usize> Default for UpdateBuffer<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> UpdateBuffer<T, N> {
    pub const MAX_CAPACITY: usize = N;

    pub const fn new() -> Self {
        Self {
            current_owner: Mutex::new(None),
            data: Mutex::new([0; N]),
        }
    }
}

impl<T: Clone, const N: usize> UpdateBuffer<T, N> {
    /// Borrow this buffer with an artifical cap of `capacity`.
    ///
    /// On success, records that this buffer is owned by `owner` until the
    /// returned buffer is dropped. On failure, returns the current owner.
    ///
    /// # Panics
    ///
    /// Panics if `capacity > N`.
    pub fn borrow(
        &self,
        new_owner: T,
        capacity: usize,
    ) -> Result<BorrowedUpdateBuffer<'_, T, N>, T> {
        if capacity > N {
            panic!();
        }

        // Lock ordering: We acquire the owner lock first.
        let mut owner = self.current_owner.lock();
        if let Some(owner) = owner.as_ref() {
            // If `owner` is `Some(_)`, we know there is an outstanding
            // `BorrowedUpdateBuffer`; return an error.
            Err(owner.clone())
        } else {
            // `owner` is `None`, which means either:
            //
            // 1. No `BorrowedUpdateBuffer` exists (this lock will succeed
            //    immediately)
            // 2. An `BorrowedUpdateBuffer` is in the process of being dropped
            //    (this lock will succeed very soon)
            //
            // Either way, we must first acquire the data lock _before_
            // unlocking `owner`, to avoid TOCTOU races with concurrent calls to
            // this method.
            //
            // This has the potential to deadlock with
            // `BorrowedUpdateBuffer::drop()`, which acquires the `owner` lock
            // while still holding the `data` lock (the reverse order from this
            // method). Normally acquiring locks in opposite order is a good way
            // to deadlock, but we avoid it in this case due to our use of
            // `owner`: We only attempt to acquire the `data` lock if `owner` is
            // `None`. If `owner` is `Some(_)`, we immediately release the lock
            // and return. If there is an outstanding `BorrowedUpdateBuffer`,
            // `owner` remains `Some(_)` until its drop method is able to
            // acquire the `owner` lock (which it will always be able to,
            // because we only hold it momentarily if it's `Some(_)`) and set it
            // back to `None`, before the drop continues and `data` is unlocked.
            let data = self.data.lock();

            // We now own the data; record our `new_owner` tag and drop the
            // owner lock. Any other caller to this method will see our owner
            // until the `BorrowedUpdateBuffer` we're about to create is dropped,
            // since `owner` will remain `Some(_)` until that drop impl runs.
            *owner = Some(new_owner);
            mem::drop(owner);

            Ok(BorrowedUpdateBuffer {
                owner: &self.current_owner,
                data,
                len: 0,
                cap: capacity,
            })
        }
    }
}

#[derive(Debug)]
pub struct BorrowedUpdateBuffer<'a, T, const N: usize> {
    owner: &'a Mutex<Option<T>>,
    data: MutexGuard<'a, [u8; N]>,
    len: usize,
    cap: usize,
}

impl<T, const N: usize> Drop for BorrowedUpdateBuffer<'_, T, N> {
    fn drop(&mut self) {
        // Avoiding deadlocks: We currently own the `data` lock and are
        // attempting to acquire the `owner` lock. This is the reverse order
        // from `UpdateBuffer::borrow()` above. See the comment in it above for
        // the reasoning why this will not deadlock.
        *self.owner.lock() = None;
        // `self.data` is dropped (and therefore unlocked) immediately after
        // this function returns, since we're in the process of being dropped.
    }
}

impl<T, const N: usize> BorrowedUpdateBuffer<'_, T, N> {
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn as_slice(&self) -> &[u8] {
        self
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Extend `self` with as much of `data` as we can, returning any remaining
    /// data. If the returned slice is empty, we extended ourselves with all of
    /// `data`.
    pub fn extend_from_slice<'a>(&mut self, data: &'a [u8]) -> &'a [u8] {
        let n = usize::min(data.len(), self.cap - self.len);
        self.data[self.len..][..n].copy_from_slice(&data[..n]);
        self.len += n;
        &data[n..]
    }

    /// Discard `self` and reborrow the underlying [`UpdateBuffer`] with a new
    /// capacity, without having to actually release and reacquire the lock.
    ///
    /// # Panics
    ///
    /// Panics if `new_capacity > N`.
    pub fn reborrow(&mut self, new_owner: T, new_capacity: usize) {
        if new_capacity > N {
            panic!();
        }

        *self.owner.lock() = Some(new_owner);
        self.len = 0;
        self.cap = new_capacity;
    }
}

impl<T, const N: usize> Deref for BorrowedUpdateBuffer<'_, T, N> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::{iter, mem, thread};

    #[test]
    fn acquiring_lock() {
        let m = UpdateBuffer::<&str, 16>::new();

        let buf1 = m.borrow("first", 4).unwrap();
        assert_eq!(m.borrow("x", 4).unwrap_err(), "first");
        mem::drop(buf1);

        let mut buf2 = m.borrow("second", 8).unwrap();
        assert_eq!(m.borrow("x", 4).unwrap_err(), "second");

        let buf3 = buf2.reborrow("third", 16);
        assert_eq!(m.borrow("x", 4).unwrap_err(), "third");

        mem::drop(buf3);
    }

    #[test]
    fn concurrent_borrow() {
        let m = Arc::new(UpdateBuffer::<String, 8>::new());

        // Spawn a bunch of threads that all hammer on the same `UpdateBuffer`
        // concurrently. We can run this both to spot check our lock ordering
        // (i.e., this test doesn't deadlock) and under miri to exercise the
        // underlying spin lock safety.
        let threads = iter::repeat(m)
            .enumerate()
            .take(16)
            .map(|(tid, m)| {
                thread::spawn(move || {
                    for i in 0..16 {
                        let id = format!("thread {tid} attempt {i}");
                        loop {
                            match m.borrow(id.clone(), 1) {
                                Ok(guard) => {
                                    // We've acquired the lock; attempting to
                                    // reacquire it should fail with our id.
                                    assert_eq!(
                                        m.borrow("foo".to_string(), 1)
                                            .unwrap_err(),
                                        id
                                    );
                                    mem::drop(guard);
                                    break;
                                }
                                Err(_) => {
                                    std::hint::spin_loop();
                                }
                            }
                        }
                        println!("{id} done");
                    }
                })
            })
            .collect::<Vec<_>>();

        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn capacity_under_max_size() {
        let m = UpdateBuffer::<&str, 16>::new();

        let mut buf = m.borrow("first", 4).unwrap();
        assert_eq!(buf.capacity(), 4);
        assert_eq!(buf.as_slice(), &[]);
        assert_eq!(buf.len(), 0);

        let leftover = buf.extend_from_slice(b"hi");
        assert_eq!(buf.capacity(), 4);
        assert_eq!(leftover, &[]);
        assert_eq!(buf.as_slice(), b"hi");
        assert_eq!(buf.len(), 2);

        let leftover = buf.extend_from_slice(b"abcd");
        assert_eq!(buf.capacity(), 4);
        assert_eq!(leftover, b"cd");
        assert_eq!(buf.as_slice(), b"hiab");
        assert_eq!(buf.len(), 4);

        mem::drop(buf);

        // Try again with capacity == max
        let mut buf = m.borrow("second", 16).unwrap();
        assert_eq!(buf.capacity(), 16);
        assert_eq!(buf.as_slice(), &[]);
        assert_eq!(buf.len(), 0);

        let leftover = buf.extend_from_slice(b"hi");
        assert_eq!(buf.capacity(), 16);
        assert_eq!(leftover, &[]);
        assert_eq!(buf.as_slice(), b"hi");
        assert_eq!(buf.len(), 2);

        let leftover = buf.extend_from_slice(b"abcd");
        assert_eq!(buf.capacity(), 16);
        assert_eq!(leftover, &[]);
        assert_eq!(buf.as_slice(), b"hiabcd");
        assert_eq!(buf.len(), 6);

        let leftover = buf.extend_from_slice(b"0123456789xyz");
        assert_eq!(buf.capacity(), 16);
        assert_eq!(leftover, b"xyz");
        assert_eq!(buf.as_slice(), b"hiabcd0123456789");
        assert_eq!(buf.len(), 16);

        // Try one more time, this time reborrowing back to a lower capacity.
        buf.reborrow("third", 3);
        assert_eq!(buf.capacity(), 3);
        assert_eq!(buf.as_slice(), &[]);
        assert_eq!(buf.len(), 0);

        let leftover = buf.extend_from_slice(b"hi");
        assert_eq!(buf.capacity(), 3);
        assert_eq!(leftover, &[]);
        assert_eq!(buf.as_slice(), b"hi");
        assert_eq!(buf.len(), 2);

        let leftover = buf.extend_from_slice(b"abcd");
        assert_eq!(buf.capacity(), 3);
        assert_eq!(leftover, b"bcd");
        assert_eq!(buf.as_slice(), b"hia");
        assert_eq!(buf.len(), 3);
    }

    #[test]
    #[should_panic]
    fn cannot_borrow_greater_than_underlying_capacity() {
        let m = UpdateBuffer::<&str, 16>::new();
        let _ = m.borrow("first", 17);
    }
}
