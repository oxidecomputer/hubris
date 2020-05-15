//! Support for safely interacting with untrusted/unprivileged/user memory.

use core::marker::PhantomData;
use zerocopy::FromBytes;

use abi::{FaultInfo, FaultSource, UsageError};
use crate::task::Task;
use crate::err::InteractFault;

pub use abi::ULease;

/// A (user, untrusted, unprivileged) slice.
///
/// A `USlice` references memory from a task, outside the kernel. The slice is
/// alleged to contain values of type `T`, but is not guaranteed to be correctly
/// aligned, etc.
///
/// The existence of a `USlice` only tells you one thing: that a task has
/// asserted that it has access to a range of memory addresses, and that the
/// addresses are correctly aligned for `T`. It does not *prove* that the task
/// has this access, that is is correctly initialized, etc. The result must be
/// used carefully.
///
/// Currently, the same `USlice` type is used for both readable and read-write
/// task memory. They are distinguished only by context. This might prove to be
/// annoying.
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
    /// This will only succeed if such a slice would not overlap the top of the
    /// address space, and if `base_address` is correctly aligned for `T`.
    ///
    /// This method will categorically reject zero-sized T.
    pub fn from_raw(
        base_address: usize,
        length: usize,
    ) -> Result<Self, UsageError> {
        // ZST check, should resolve at compile time:
        uassert!(core::mem::size_of::<T>() != 0);

        // Alignment check:
        if base_address % core::mem::align_of::<T>() != 0 {
            return Err(UsageError::InvalidSlice);
        }
        // Check that a slice of `length` `T`s can even exist starting at
        // `base_address`, without wrapping around. This check is slightly
        // complicated by a desire to _allow_ slices that end at the top of the
        // address space.
        let size_in_bytes = length
            .checked_mul(core::mem::size_of::<T>())
            .ok_or(UsageError::InvalidSlice)?;
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

    /// Returns the *highest* address in this slice, inclusive.
    ///
    /// This produces `None` if the slice is empty.
    pub fn last_byte_addr(&self) -> Option<usize> {
        // This implementation would be wrong for ZSTs, but we blocked them at
        // construction.
        let size_in_bytes = self.length * core::mem::size_of::<T>();
        if size_in_bytes == 0 {
            None
        } else {
            Some(
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

    /// Copies out element `index` from the slice, if `index` is in range.
    /// Otherwise returns `None`.
    ///
    /// # Safety
    ///
    /// To read data from the slice safely, you must be certain that it reflects
    /// readable non-kernel memory. The easiest way to ensure this is to check
    /// `Task::can_read` (assuming the application's memory regions don't
    /// overlap the kernel).
    ///
    /// The read is *not* performed using `volatile`, so this is not appropriate
    /// for accessing device registers.
    pub unsafe fn get(&self, index: usize) -> Option<T>
        where T: Copy
    {
        if index < self.length {
            Some((self.base_address as *const T).add(index).read())
        } else {
            None
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
    /// # Safety
    ///
    /// This operation is totally unchecked, so to use it safely, you must first
    /// convince yourself of the following.
    ///
    /// 1. That the memory region this `USlice` describes is actual memory.
    /// 2. That this memory is legally readable by whatever task you're doing
    ///    work on behalf of.
    /// 3. That it is correctly aligned for type `T`.
    /// 4. That it contains bytes that are valid `T`s. (The `FromBytes`
    ///    constraint ensures this statically.)
    /// 5. That it does not alias any slice you intend to `&mut`-reference with
    ///    `assume_writable`, or any kernel memory.
    pub unsafe fn assume_readable(&self) -> &[T] {
        core::slice::from_raw_parts(self.base_address as *const T, self.length)
    }

    /// Converts this into an _actual_ slice that can be directly read and
    /// written by the kernel.
    ///
    /// # Safety
    ///
    /// This operation is totally unchecked, so to use it safely, you must first
    /// convince yourself of the following:
    ///
    /// 1. That the memory region this `USlice` describes is actual memory.
    /// 2. That this memory is legally writable by whatever task you're doing
    ///    work on behalf of.
    /// 3. That it is correctly aligned for type `T`.
    /// 4. That it contains bytes that are valid `T`s. (The `FromBytes`
    ///    constraint ensures this statically.)
    /// 5. That it does not alias any other slice you intend to access, or any
    ///    kernel memory.
    pub unsafe fn assume_writable(&mut self) -> &mut [T] {
        core::slice::from_raw_parts_mut(
            self.base_address as *mut T,
            self.length,
        )
    }
}

/// Extracts the base/bound part of a `ULease` as a `USlice` of bytes.
impl<'a> From<&'a ULease> for USlice<u8> {
    fn from(lease: &'a ULease) -> Self {
        Self {
            base_address: lease.base_address,
            length: lease.length,
            _marker: PhantomData,
        }
    }
}

/// Copies bytes from task `from` in region `from_slice` into task `to` at
/// region `to_slice`, checking memory access before doing so.
///
/// The actual number of bytes copied will be `min(from_slice.length,
/// to_slice.length)`, and will be returned.
///
/// If `from_slice` or `to_slice` refers to memory the task can't read or write
/// (respectively), no bytes are copied, and this returns an `InteractFault`
/// indicating which task(s) messed this up.
///
/// This operation will not accept device memory as readable or writable.
pub fn safe_copy(
    from: &Task,
    from_slice: USlice<u8>,
    to: &Task,
    mut to_slice: USlice<u8>,
) -> Result<usize, InteractFault> {
    let src_fault = if from.can_read(&from_slice) {
        None
    } else {
        Some(FaultInfo::MemoryAccess {
            address: Some(from_slice.base_address),
            source: FaultSource::Kernel,
        })
    };
    // We're going to blame any aliasing on the recipient, who shouldn't have
    // designated a receive buffer in shared memory. This decision is somewhat
    // arbitrary.
    let dst_fault = if to.can_write(&to_slice) && !from_slice.aliases(&to_slice)
    {
        None
    } else {
        Some(FaultInfo::MemoryAccess {
            address: Some(to_slice.base_address),
            source: FaultSource::Kernel,
        })
    };
    if src_fault.is_some() || dst_fault.is_some() {
        return Err(InteractFault {
            src: src_fault,
            dst: dst_fault,
        });
    }

    // We are now convinced, after querying the tasks, that these RAM areas are
    // legit.
    // TODO: this next bit assumes that task memory is directly addressable --
    // an assumption that is likely to be invalid in a simulator.
    let copy_len = from_slice.len().min(to_slice.len());
    let from = unsafe { from_slice.assume_readable() };
    let to = unsafe { to_slice.assume_writable() };
    to[..copy_len].copy_from_slice(&from[..copy_len]);
    Ok(copy_len)
}
