use core::marker::PhantomData;
use crate::task::Task;

/// A (user, untrusted, unprivileged) slice.
///
/// A `USlice` references memory from a task, outside the kernel. The slice is
/// alleged to contain values of type `T`, but is not guaranteed to be correctly
/// aligned, etc.
///
/// The existence of a `USlice` only tells you one thing: that a task has
/// asserted that it has access to a range of memory addresses. It does not
/// *prove* that the task has this access, that it is aligned, that is is
/// correctly initialized, etc. The result must be used carefully.
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
    pub fn from_raw(base_address: usize, length: usize) -> Self {
        Self { base_address, length, _marker: PhantomData }
    }

    /// Returns the number of `T`s in this slice.
    pub fn len(&self) -> usize {
        self.length
    }
}

/// Structure describing a lease in task memory. This is an ABI commitment.
///
/// At SEND, the task gives us the base and length of a section of memory that
/// it *claims* contains structs of this type.
#[derive(Debug)]
#[repr(C)]
pub struct ULease {
    /// Lease attributes.
    ///
    /// Currently, bit 0 indicates readable memory, and bit 1 indicates writable
    /// memory. All other bits are currently undefined and should be zero.
    pub attributes: u32,
    /// Base address of leased memory. This is equivalent to the base address
    /// field in `USlice`, but isn't represented as a `USlice` because we leave
    /// the internal memory representation of `USlice` out of the ABI.
    pub base_address: usize,
    /// Length of leased memory, in bytes.
    pub length: usize,
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
/// (respectively), no bytes are copied, and this returns a `CopyError`
/// indicating which task(s) messed this up.
pub fn safe_copy(
    _from: &Task,
    _from_slice: USlice<u8>,
    _to: &Task,
    _to_slice: USlice<u8>,
) -> Result<usize, CopyError> {
    unimplemented!()
}

/// Failure information returned from `safe_copy`.
///
/// The faulting addresses returned in this struct provide *examples* of an
/// illegal address. The precise choice of faulting address within a bad slice
/// is left undefined.
pub struct CopyError {
    /// Address where source would have faulted.
    pub src_fault: Option<usize>,
    /// Address where dest would have faulted.
    pub dest_fault: Option<usize>,
}
