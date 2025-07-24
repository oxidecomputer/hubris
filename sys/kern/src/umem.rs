// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Support for safely interacting with untrusted/unprivileged/user memory.

use core::marker::PhantomData;
use core::ops::Range;
use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::err::InteractFault;
use crate::task::Task;
use crate::util::index2_distinct;
use abi::{FaultInfo, FaultSource, UsageError};

/// A (user, untrusted, unprivileged) slice.
///
/// A `USlice` is passed into the kernel by a task, and is intended to refer to
/// memory that task controls -- for instance, as a place where the kernel can
/// deposit a message to that task. However, the `USlice` type itself simply
/// represents an _allegation_ from the task that a section of address space is
/// suitable; it does _not_ demonstrate that the task has access to that memory.
/// It could point into the kernel, to peripherals, etc.
///
/// Having a `USlice<T>` tells you the following:
///
/// - Some task has claimed it has access to a section of address space
///   (delimited by the `USlice`).
/// - The base of the section is correctly aligned for type `T`.
/// - The section does not wrap around the end of the address space.
///
/// To actually access the memory referred to by a `USlice`, you need to hand it
/// to `Task::try_read` or `Task::try_write` to validate it.
///
/// Note that this same `USlice` type is used for both readable and read-write
/// contexts -- there is no `USliceMut`. So far, this has not seemed like a
/// decision that will generate bugs.
pub struct USlice<T> {
    /// Base address of the slice.
    base_address: usize,
    /// Number of `T` elements in the slice.
    length: usize,
    /// since we don't actually use T...
    _marker: PhantomData<*mut [T]>,
}

impl<T> USlice<T> {
    /// Constructs a `USlice` given a base address and length passed from
    /// untrusted code.
    ///
    /// This will only succeed if such a slice would not overlap or touch the
    /// top of the address space, and if `base_address` is correctly aligned for
    /// `T`.
    ///
    /// This method will categorically reject zero-sized T.
    pub fn from_raw(
        base_address: usize,
        length: usize,
    ) -> Result<Self, UsageError> {
        // NOTE: the properties checked here are critical for the correctness of
        // this type. Think carefully before loosening any of them, or adding a
        // second way to construct a USlice.

        // ZST check, should resolve at compile time:
        uassert!(core::mem::size_of::<T>() != 0);

        // Alignment check:
        if !base_address.is_multiple_of(core::mem::align_of::<T>()) {
            return Err(UsageError::InvalidSlice);
        }
        // Check that a slice of `length` `T`s can even exist starting at
        // `base_address`, without wrapping around.
        let size_in_bytes = length
            .checked_mul(core::mem::size_of::<T>())
            .ok_or(UsageError::InvalidSlice)?;
        // Note: this subtraction cannot underflow. You can subtract any usize
        // from usize::MAX.
        let highest_possible_base = usize::MAX - size_in_bytes;
        if base_address <= highest_possible_base {
            Ok(Self {
                base_address,
                length,
                _marker: PhantomData,
            })
        } else {
            Err(UsageError::InvalidSlice)
        }
    }

    /// Constructs an empty `USlice`.
    ///
    /// This ensures that the base address is not zero and is properly aligned,
    /// despite the length being zero, so that it's safe to turn into an empty
    /// slice.
    pub fn empty() -> Self {
        Self {
            base_address: core::ptr::NonNull::<T>::dangling().as_ptr() as usize,
            length: 0,
            _marker: PhantomData,
        }
    }

    /// Returns `true` if this slice is zero-length, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns the number of `T`s in this slice.
    pub fn len(&self) -> usize {
        self.length
    }

    /// Returns the bottom address of this slice as a `usize`.
    pub fn base_addr(&self) -> usize {
        self.base_address
    }

    /// Returns the end address of the slice, which is the address one past its
    /// final byte -- or its base address if it's empty.
    pub fn end_addr(&self) -> usize {
        // Compute the size using an unchecked multiplication. Why can we do
        // this? Because we checked that this multiplication does not overflow
        // at construction above. Using an unchecked multiply here removes some
        // instructions.
        let size_in_bytes = self.length.wrapping_mul(core::mem::size_of::<T>());
        self.base_address.wrapping_add(size_in_bytes)
    }

    /// Returns the *highest* address in this slice, inclusive.
    ///
    /// This produces `None` if the slice is empty.
    pub fn last_byte_addr(&self) -> Option<usize> {
        // This implementation would be wrong for ZSTs (it would indicate any
        // slice of ZSTs as empty), but we blocked them at construction.

        // Compute the size using an unchecked multiplication. Why can we do
        // this? Because we checked that this multiplication does not overflow
        // at construction above. Using an unchecked multiply here removes some
        // instructions.
        let size_in_bytes = self.length.wrapping_mul(core::mem::size_of::<T>());
        if size_in_bytes == 0 {
            None
        } else {
            Some(
                // Note: wrapping operations are safe here because we checked
                // that the slice doesn't overlap the end of the address space
                // at construction.
                self.base_address
                    .wrapping_add(size_in_bytes)
                    .wrapping_sub(1),
            )
        }
    }

    /// Checks whether this slice aliases (overlaps) `other`.
    ///
    /// Empty slices alias no slices, including themselves.
    pub fn aliases(&self, other: &Self) -> bool {
        // This test is made slightly involved by a desire to support slices
        // that end at the top of the address space. We've already verified at
        // construction that the range is valid.

        match (self.last_byte_addr(), other.last_byte_addr()) {
            (Some(self_end), Some(other_end)) => {
                self_end >= other.base_address && other_end >= self.base_address
            }
            // One slice or the other was empty
            _ => false,
        }
    }

    /// Adjusts `a` and `b` to have the same length, which is the shorter of the
    /// two.
    ///
    /// Returns the new common length.
    ///
    /// Shortening `a` and `b` ensures that the slices returned from `try_read`
    /// / `try_write` have the same length, without the need for further
    /// slicing.
    pub fn shorten_to_match(a: &mut Self, b: &mut Self) -> usize {
        let n = usize::min(a.length, b.length);
        a.length = n;
        b.length = n;
        n
    }
}

impl<T> USlice<T>
where
    T: FromBytes + Immutable + KnownLayout,
{
    /// Converts this into an _actual_ slice that can be directly read by the
    /// kernel.
    ///
    /// If you are implementing a syscall, please have a look at
    /// `Task::try_read` instead.
    ///
    /// # Safety
    ///
    /// This operation is totally unchecked, so to use it safely, you must first
    /// convince yourself of the following.
    ///
    /// 1. That the memory region this `USlice` describes is actual memory.
    /// 2. That this memory is legally readable by whatever task you're doing
    ///    work on behalf of.
    /// 3. That it contains bytes that are valid `T`s. (The `FromBytes, Immutable, KnownLayout`
    ///    constraint ensures this statically.)
    /// 4. That it does not alias any slice you intend to `&mut`-reference with
    ///    `assume_writable`, or any kernel memory.
    pub unsafe fn assume_readable(&self) -> &[T] {
        // Safety: this function's contract ensures that the slice we produce
        // here is valid.
        unsafe {
            core::slice::from_raw_parts(
                self.base_address as *const T,
                self.length,
            )
        }
    }

    /// Converts this into an _actual_ slice that can be directly read and
    /// written by the kernel.
    ///
    /// If you are implementing a syscall, please have a look at
    /// `Task::try_write` instead.
    ///
    /// # Safety
    ///
    /// This operation is totally unchecked, so to use it safely, you must first
    /// convince yourself of the following:
    ///
    /// 1. That the memory region this `USlice` describes is actual memory.
    /// 2. That this memory is legally writable by whatever task you're doing
    ///    work on behalf of.
    /// 3. That it contains bytes that are valid `T`s. (The `FromBytes, Immutable, KnownLayout`
    ///    constraint ensures this statically.)
    /// 4. That it does not alias any other slice you intend to access, or any
    ///    kernel memory.
    pub unsafe fn assume_writable(&mut self) -> &mut [T] {
        // Safety: this function's contract ensures that the slice we produce
        // here is valid.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.base_address as *mut T,
                self.length,
            )
        }
    }

    /// Converts this into a raw slice, which could be used for raw pointer
    /// accesses.
    ///
    /// If you are implementing a syscall, please have a look at
    /// `Task::try_read_dma` instead.
    ///
    /// # Safety
    ///
    /// This operation is totally unchecked, so to use it safely, you must first
    /// convince yourself of the following.
    ///
    /// 1. That the memory region this `USlice` describes is actual memory.
    /// 2. That this memory is legally readable by whatever task you're doing
    ///    work on behalf of.
    /// 3. That it contains bytes that are valid `T`s. (The `FromBytes, Immutable, KnownLayout`
    ///    constraint ensures this statically.)
    /// 4. That it does not alias any slice you intend to `&mut`-reference with
    ///    `assume_writable`, or any kernel memory.
    pub unsafe fn assume_readable_raw(&self) -> Range<*const T> {
        let p = self.base_address as *const T;
        // Safety: this is unsafe because the pointer addition might overflow.
        // It won't though, due to the invariants on this type and the required
        // preconditions for this function.
        unsafe { p..p.add(self.length) }
    }
}

impl<T> Clone for USlice<T> {
    fn clone(&self) -> Self {
        Self {
            base_address: self.base_address,
            length: self.length,
            _marker: PhantomData,
        }
    }
}

/// Can't `derive(Debug)` for `USlice` because that puts a `Debug` requirement
/// on `T`, and that's silly.
impl<T> core::fmt::Debug for USlice<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("USlice")
            .field("base_address", &self.base_address)
            .field("length", &self.length)
            .finish()
    }
}

/// Extracts the base/bound part of a `ULease` as a `USlice` of bytes.
impl<'a> From<&'a abi::ULease> for USlice<u8> {
    fn from(lease: &'a abi::ULease) -> Self {
        Self {
            base_address: lease.base_address as usize,
            length: lease.length as usize,
            _marker: PhantomData,
        }
    }
}

/// Compatibility with the generic portable algorithms in `kerncore`.
impl<T> kerncore::UserSlice for USlice<T> {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn base_addr(&self) -> usize {
        self.base_addr()
    }

    fn end_addr(&self) -> usize {
        self.end_addr()
    }
}

/// Copies bytes from `tasks[from_index]` in region `from_slice` into
/// `tasks[to_index]` at region `to_slice`, checking memory access before doing
/// so.
///
/// The actual number of bytes copied will be `min(from_slice.length,
/// to_slice.length)`, and will be returned.
///
/// If `from_slice` or `to_slice` refers to memory that the respective task
/// can't read or write (respectively), no bytes are copied, and this returns an
/// `InteractFault` indicating which task(s) messed this up. Note that it's
/// entirely possible for _both_ tasks to have messed this up.
///
/// This operation will not operate on (read or write) memory marked as
/// any combination of `DEVICE` and `DMA`, as a side effect of its use of `Task`
/// API to validate the memory regions.
pub fn safe_copy(
    tasks: &mut [Task],
    from_index: usize,
    mut from_slice: USlice<u8>,
    to_index: usize,
    mut to_slice: USlice<u8>,
) -> Result<usize, InteractFault> {
    let copy_len = USlice::shorten_to_match(&mut from_slice, &mut to_slice);

    if copy_len == 0 {
        // try_read and try_write both accept _any_ empty slice, which then
        // results in a zero-byte copy. We can skip some steps.
        return Ok(0);
    }

    let (from, to) = index2_distinct(tasks, from_index, to_index);

    let src = from.try_read(&from_slice);
    // We're going to blame any aliasing on the recipient, who shouldn't have
    // designated a receive buffer in shared memory. This decision is somewhat
    // arbitrary.
    let dst = if from_slice.aliases(&to_slice) {
        Err(FaultInfo::MemoryAccess {
            address: Some(to_slice.base_address as u32),
            source: FaultSource::Kernel,
        })
    } else {
        to.try_write(&mut to_slice)
    };

    match (src, dst) {
        (Ok(from), Ok(to)) => {
            // We are now convinced, after querying the tasks, that these RAM
            // areas are legit.
            to.copy_from_slice(from);
            Ok(copy_len)
        }
        (src, dst) => Err(InteractFault {
            src: src.err(),
            dst: dst.err(),
        }),
    }
}

/// Variation on `safe_copy` that is willing to read (but not write) DMA memory.
///
/// Otherwise, see `safe_copy` for prerequisites and docs.
///
/// Writing DMA could be enabled without obvious safety implications at the time
/// of this writing, but since we currently don't need it, I've left it
/// prevented here.
pub fn safe_copy_dma(
    tasks: &mut [Task],
    from_index: usize,
    from_slice: USlice<u8>,
    to_index: usize,
    mut to_slice: USlice<u8>,
) -> Result<usize, InteractFault> {
    let copy_len = from_slice.len().min(to_slice.len());

    let (from, to) = index2_distinct(tasks, from_index, to_index);

    let src = from.try_read_dma(&from_slice);
    // We're going to blame any aliasing on the recipient, who shouldn't have
    // designated a receive buffer in shared memory. This decision is somewhat
    // arbitrary.
    let dst = if from_slice.aliases(&to_slice) {
        Err(FaultInfo::MemoryAccess {
            address: Some(to_slice.base_address as u32),
            source: FaultSource::Kernel,
        })
    } else {
        to.try_write(&mut to_slice)
    };

    match (src, dst) {
        (Ok(from), Ok(to)) => {
            // We are now convinced, after querying the tasks, that these RAM
            // areas are legit.
            //
            // Safety: copy_nonoverlapping is unsafe because it can do arbitrary
            // memory-to-memory transfers. In this case, we've checked that both
            // the source and destination addresses are valid, and rounded down
            // the transfer length to the common prefix, so the copy should be
            // sound.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    from.start,
                    to.as_mut_ptr(),
                    copy_len,
                );
            }

            Ok(copy_len)
        }
        (src, dst) => Err(InteractFault {
            src: src.err(),
            dst: dst.err(),
        }),
    }
}
