// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Support for safely interacting with untrusted/unprivileged/user memory.

use core::marker::PhantomData;
use zerocopy::FromBytes;

use crate::err::InteractFault;
use crate::task::Task;
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
        if base_address % core::mem::align_of::<T>() != 0 {
            return Err(UsageError::InvalidSlice);
        }
        // Check that a slice of `length` `T`s can even exist starting at
        // `base_address`, without wrapping around.
        let size_in_bytes = length
            .checked_mul(core::mem::size_of::<T>())
            .ok_or(UsageError::InvalidSlice)?;
        // Note: this subtraction cannot underflow. You can subtract any usize
        // from usize::MAX.
        let highest_possible_base = core::usize::MAX - size_in_bytes;
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
}

impl<T> USlice<T>
where
    T: FromBytes,
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
    /// 3. That it contains bytes that are valid `T`s. (The `FromBytes`
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
    /// 3. That it contains bytes that are valid `T`s. (The `FromBytes`
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
    from_slice: USlice<u8>,
    to_index: usize,
    mut to_slice: USlice<u8>,
) -> Result<usize, InteractFault> {
    let copy_len = from_slice.len().min(to_slice.len());

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
            to[..copy_len].copy_from_slice(&from[..copy_len]);
            Ok(copy_len)
        }
        (src, dst) => Err(InteractFault {
            src: src.err(),
            dst: dst.err(),
        }),
    }
}

/// Utility routine for getting `&mut` to _two_ elements of a slice, at indexes
/// `i` and `j`. `i` and `j` must be distinct, or this will panic.
#[allow(clippy::comparison_chain)]
fn index2_distinct<T>(
    elements: &mut [T],
    i: usize,
    j: usize,
) -> (&mut T, &mut T) {
    if i < j {
        let (prefix, suffix) = elements.split_at_mut(i + 1);
        (&mut prefix[i], &mut suffix[j - (i + 1)])
    } else if j < i {
        let (prefix, suffix) = elements.split_at_mut(j + 1);
        (&mut suffix[i - (j + 1)], &mut prefix[j])
    } else {
        panic!()
    }
}
