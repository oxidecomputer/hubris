//! Support for safely interacting with untrusted/unprivileged/user memory.

use core::marker::PhantomData;
use zerocopy::FromBytes;

use crate::task::{FaultInfo, FaultSource, Task, UsageError};
use crate::InteractFault;

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
        assert!(core::mem::size_of::<T>() != 0);

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

    /// Returns the number of `T`s in this slice.
    pub fn len(&self) -> usize {
        self.length
    }

    pub fn base_addr(&self) -> usize {
        self.base_address
    }

    /// Returns the *highest* address in this slice, inclusive.
    pub fn last_byte_addr(&self) -> usize {
        // This implementation would be wrong for ZSTs, but we blocked them at
        // construction.
        let size_in_bytes = self.length * core::mem::size_of::<T>();
        self.base_address
            .wrapping_add(size_in_bytes)
            .wrapping_sub(1)
    }

    /// Checks whether this slice aliases (overlaps) `other`.
    pub fn aliases(&self, other: &Self) -> bool {
        // This test is made slightly involved by a desire to support slices
        // that end at the top of the address space. We've already verified at
        // construction that the range is valid.
        let self_end = self.last_byte_addr();
        let other_end = other.last_byte_addr();

        self_end >= other.base_address && other_end >= self.base_address
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
    ///    `assume_writable`.
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
    /// 5. That it does not alias any other slice you intend to access.
    pub unsafe fn assume_writable(&mut self) -> &mut [T] {
        core::slice::from_raw_parts_mut(
            self.base_address as *mut T,
            self.length,
        )
    }
}

/// Structure describing a lease in task memory. This is an ABI commitment.
///
/// At SEND, the task gives us the base and length of a section of memory that
/// it *claims* contains structs of this type.
#[derive(Debug, FromBytes)]
#[repr(C)]
pub struct ULease {
    /// Lease attributes.
    pub attributes: LeaseAttributes,
    /// Base address of leased memory. This is equivalent to the base address
    /// field in `USlice`, but isn't represented as a `USlice` because we leave
    /// the internal memory representation of `USlice` out of the ABI.
    pub base_address: usize,
    /// Length of leased memory, in bytes.
    pub length: usize,
}

bitflags::bitflags! {
    #[derive(FromBytes)]
    #[repr(transparent)]
    pub struct LeaseAttributes: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
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
            sender: src_fault,
            recipient: dst_fault,
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
