//! Support for safely interacting with untrusted/unprivileged/user memory.

use core::marker::PhantomData;

use abi::{FaultInfo, FaultSource, UsageError};
use crate::task::Task;
use crate::err::InteractFault;

pub use abi::ULease;

/// Marker trait for data types that can be safely shared with unprivileged
/// memory.
///
/// This is implemented for many of the primitive types, and can be implemented
/// for your own types.
///
/// # Requirements for implementing `UShared`
///
/// This is an `unsafe` trait, meaning it's unsafe to implement without care.
/// Any type `T` that implements `UShared` must meet the following requirements:
///
/// 1. **Valid for all inputs.** Any sequence of bytes that is `sizeof::<T>()`
///    long must be a valid `T`. For instance, `u32` meets this requirement,
///    while `bool` does not.
///
/// 2. **Does not contain references (`&` or `&mut`).** This actually just
///    follows from item 1, because it is illegal in Rust to have a reference
///    that is zero, misaligned, or pointing to uninitialized memory, even if
///    you don't dereference it -- references are not valid for all inputs.
///    (Note that `UShared` types *may* contain raw pointers.)
///
/// In general, any `struct` that contains only fields of `UShared` types can
/// potentially implement `UShared`. Implementing `UShared` for an `enum` is
/// almost certainly unsafe, because it allows user programs to introduce
/// illegal values for the enum discriminator.
///
/// There's one more *soft* requirement, which is that a type implementing
/// `UShared` *should* have a stable ABI. There's no guarantee that the kernel
/// and tasks were compiled with the same version of `rustc` (though this is the
/// normal case). This means that it's possible the compilers would choose
/// different layouts for struct types. You can avoid this by using
///
/// - Primitive types like `u32`, or their `Atomic` equivalents,
/// - `#[repr(C)]` types containing other `UShared` types (including arrays),
/// - `#[repr(transparent)]` types wrapping one of the above.
///
/// Note that the memory layout of tuples is explicitly not stable. Use
/// `#[repr(C)]` structs instead.
///
/// See the [data layout] section of the Unsafe Code Guidelines for more.
///
/// [data layout]: https://rust-lang.github.io/unsafe-code-guidelines/layout.html
///
/// # "Exclusive" references
///
/// There are operations in the `umem` module that can produce an `&mut T` for
/// `T: UShared`. This appears to break the aliasing rules for Rust `&mut`,
/// because the task can also access the value.  However, remember that the
/// kernel is a separate program from the tasks, running independently; tasks
/// cannot access memory while the kernel is running, and task memory is, from
/// the kernel's point of view, just big byte arrays.
///
/// What *is* important is that the *kernel* does not produce two `&mut T`s that
/// overlap or alias. You must ensure this yourself, which is why the operations
/// for accessing a `UShared` type are `unsafe`.
///
/// # Similarity to `zerocopy`
///
/// `UShared` provides a similar, but not equivalent, set of guarantees as the
/// `zerocopy::FromBytes` trait: both mark types that are valid for any byte
/// sequence of the correct length. The key difference is that `UShared` does
/// *not* provide a way to convert a `&[u8]` into a `T`.
///
/// This is important, because there are some types in Rust that are unsafe to
/// alias with a `&[u8]` -- particularly the `Atomic` family of types, because
/// having atomic and non-atomic accesses to the same byte in memory can cause
/// data races. (Rumor has it that `UnsafeCell` is also problematic, but I
/// haven't found an explanation of *why.*)
pub unsafe trait UShared {}

/// Trait that unites real slices like `&[u8]` and unprivileged slices like
/// `USlice`, since both have an extent in memory.
pub trait MemoryExtent {
    type Element;

    fn base_addr(&self) -> usize;
    fn len(&self) -> usize;

    fn last_byte_addr(&self) -> Option<usize> {
        let size_in_bytes = self.len() * core::mem::size_of::<Self::Element>();
        if size_in_bytes == 0 {
            None
        } else {
            Some(
                self.base_addr()
                    .wrapping_add(size_in_bytes)
                    .wrapping_sub(1),
            )
        }
    }

    /// Checks whether this slice aliases (overlaps) `other`.
    ///
    /// Empty slices alias no slices, including themselves.
    fn aliases(&self, other: &impl MemoryExtent) -> bool {
        // This test is made slightly involved by a desire to support slices
        // that end at the top of the address space. We expect slice
        // constructors to filter out slices that *cross* the end.

        match (self.last_byte_addr(), other.last_byte_addr()) {
            (Some(self_end), Some(other_end)) => {
                self_end >= other.base_addr() && other_end >= self.base_addr()
            }
            // One slice or the other was empty
            _ => false,
        }
    }
}

impl<T> MemoryExtent for [T] {
    type Element = T;

    fn base_addr(&self) -> usize {
        self.as_ptr() as usize
    }
    fn len(&self) -> usize {
        self.len()
    }
}

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
    T: UShared,
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
    /// 4. That it contains bytes that are valid `T`s. (The `UShared` constraint
    ///    ensures this statically.)
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
    /// 4. That it contains bytes that are valid `T`s. (The `UShared` constraint
    ///    ensures this statically.)
    /// 5. That it does not alias any other slice you intend to access, or any
    ///    kernel memory.
    pub unsafe fn assume_writable(&mut self) -> &mut [T] {
        core::slice::from_raw_parts_mut(
            self.base_address as *mut T,
            self.length,
        )
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
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("USlice")
            .field("base_address", &self.base_address)
            .field("length", &self.length)
            .finish()
    }
}

/// Extracts the base/bound part of a `ULease` as a `USlice` of bytes.
impl<'a> From<&'a ULease> for USlice<u8> {
    fn from(lease: &'a ULease) -> Self {
        Self {
            base_address: lease.base_address as usize,
            length: lease.length as usize,
            _marker: PhantomData,
        }
    }
}

impl<T> MemoryExtent for USlice<T> {
    type Element = T;

    fn base_addr(&self) -> usize {
        self.base_address
    }
    fn len(&self) -> usize {
        self.length
    }
}

unsafe impl<T> UShared for USlice<T> {}

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
    to_slice: USlice<u8>,
) -> Result<usize, InteractFault> {
    // We're going to fault the source immediately. This means the recipient
    // might not fault, despite having invalid response buffers. Fortunately,
    // the IPC primitives check for that, so you'd need to work pretty hard to
    // get there. This makes the factoring of the code a lot easier.
    if !from.can_read(&from_slice) {
        return Err(InteractFault::in_src(
            FaultInfo::MemoryAccess {
                address: Some(from_slice.base_address as u32),
                source: FaultSource::Kernel,
            }
        ));
    }

    let from = unsafe { from_slice.assume_readable() };

    safe_copy_from(from, to, to_slice)
        .map_err(InteractFault::in_dst)
}

/// Copies bytes from known-readable slice `from_slice` ice` into task `to` at
/// region `to_slice`, checking memory access before doing so.
///
/// The actual number of bytes copied will be `min(from_slice.len(),
/// to_slice.length)`, and will be returned.
///
/// If `to_slice` refers to memory its owner can't write, no bytes are copied,
/// and this returns a `FaultInfo` to be assigned to the `to` task.
///
/// This operation will not accept device memory as writable.
pub fn safe_copy_from(
    from_slice: &[u8],
    to: &Task,
    mut to_slice: USlice<u8>,
) -> Result<usize, FaultInfo> {
    // We're going to blame any aliasing on the recipient, who shouldn't have
    // designated a receive buffer in shared memory. This decision is somewhat
    // arbitrary.
    if !to.can_write(&to_slice) || from_slice.aliases(&to_slice) {
        return Err(FaultInfo::MemoryAccess {
            address: Some(to_slice.base_address as u32),
            source: FaultSource::Kernel,
        })
    }

    // We are now convinced, after querying the tasks, that these RAM areas are
    // legit.
    let copy_len = from_slice.len().min(to_slice.len());
    let to = unsafe { to_slice.assume_writable() };
    to[..copy_len].copy_from_slice(&from_slice[..copy_len]);
    Ok(copy_len)
}

//
// The big list of external types that have UShared impls!
//
// This list should contain only the impls for external crates and/or the
// standard library (incl. abi).
//
// It should *not* contain impls for kernel types. Those should go with the type
// definitions.
//

unsafe impl UShared for () {}

unsafe impl UShared for u8 {}
unsafe impl UShared for u16 {}
unsafe impl UShared for u32 {}
unsafe impl UShared for u64 {}
unsafe impl UShared for u128 {}

unsafe impl UShared for i8 {}
unsafe impl UShared for i16 {}
unsafe impl UShared for i32 {}
unsafe impl UShared for i64 {}
unsafe impl UShared for i128 {}

unsafe impl UShared for core::sync::atomic::AtomicU8 {}
unsafe impl UShared for core::sync::atomic::AtomicU16 {}
unsafe impl UShared for core::sync::atomic::AtomicU32 {}
// Our target architectures do not have atomic operations > 32 bits

unsafe impl UShared for core::sync::atomic::AtomicI8 {}
unsafe impl UShared for core::sync::atomic::AtomicI16 {}
unsafe impl UShared for core::sync::atomic::AtomicI32 {}
// Our target architectures do not have atomic operations > 32 bits

// It is *not* safe to impl UShared for bool!

unsafe impl<T> UShared for *const T {}
unsafe impl<T> UShared for *mut T {}
unsafe impl<T> UShared for core::sync::atomic::AtomicPtr<T> {}
// It is *not* safe to impl UShared for NonNull<T>!

unsafe impl UShared for abi::AsyncDesc {}
unsafe impl UShared for abi::LeaseAttributes {}
unsafe impl UShared for abi::TaskId {}
unsafe impl UShared for abi::ULease {}
