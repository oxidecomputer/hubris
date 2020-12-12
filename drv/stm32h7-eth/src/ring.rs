//! Descriptor ring buffer implementations.
//!
//! The thing we're calling a "descriptor ring buffer" is a data structure that
//! has an array of *descriptors* that can be used to send commands to the
//! Ethernet DMA engine, and an array of *buffers* that can be used to hold
//! Ethernet frame data.
//!
//! # Descriptor ownership and state changes
//!
//! This module has no opinions about what a "descriptor" is or how it is
//! represented -- just that it implement the `Descriptor` trait.
//!
//! The only property of a descriptor that this module really cares about is its
//! _state._ A descriptor can be in three states:
//!
//! - Owned by the hardware.
//! - Free.
//! - In use by the driver.
//!
//! When a descriptor is owned by the hardware, the Ethernet hardware may read
//! and write it at any time. This condition is indicated by an OWN bit in the
//! descriptor. Software sets the OWN bit as part of the process of handing the
//! descriptor to the hardware. The hardware indicates that it's through messing
//! with a descriptor by clearing the OWN bit.
//!
//! When the OWN bit is clear, the descriptor is either free, or being used by
//! software. The hardware doesn't make this distinction, and no state is
//! written to memory -- _we_ make this distinction to avoid accidental aliasing
//! and whatnot. The `Ring` API is structured such that the borrow checker
//! enforces this.
//!
//! From a Rust perspective, the hardware acts as another mutator/observer of
//! the descriptor. This has some implications on safety:
//!
//! 1. We _cannot_ be in possession of an exclusive reference (`&mut`) to a
//!    descriptor while it is OWNed, simply because our reference is not truly
//!    exclusive -- the hardware holds one and might change data behind our
//!    backs.
//!
//! 2. We _can_ hold a shared reference (`&`) to a descriptor while it is OWNed,
//!    but _only_ if the fields the hardware might change are marked as
//!    interior-mutable using a `Cell` type.
//!
//! The approach we've chosen for this API is:
//!
//! - Descriptor contents are stored in `VolatileCell`.
//! - We will materialize shared references to descriptors to check their
//! status, but these do not leak outside this module.
//! - We will _hand out_ `&mut` exclusive references to descriptors, but only
//!   when they've been verified to be non-OWNed, and only in a way that
//!   prevents them from escaping or being stored somewhere.
//!
//! # Buffer management
//!
//! Descriptors reference some associated data buffer, which we treat as a
//! `[u8]`. The `Ring` struct _owns_ those data buffers. To simplify memory
//! management, we operate in terms of two parallel arrays of equal size. Each
//! descriptor is permanently associated with the corresponding buffer. We could
//! do something more complex than this, but given the way the driver interacts
//! with the network stack, this simpler approach appears sufficient -- and it's
//! _much simpler._

use core::mem::MaybeUninit;

/// Trait implemented by types that can be used as descriptors with a `Ring`.
///
/// This exposes the narrow set of descriptor operations `Ring` requires, which
/// are concerned only with initializing values and checking state.
///
/// This is an `unsafe` trait because implementing it incorrectly can compromise
/// the memory safety of the `Ring`. Follow the instructions below -- in
/// particular, don't set the `OWN` bit anywhere but in `set_owned_by_hw`.
pub unsafe trait Descriptor: 'static {
    /// Produces an appropriate initial state for a descriptor that is bonded
    /// with the given buffer. The resulting descriptor will typically stash the
    /// buffer's address so the hardware can find it.
    ///
    /// Implementations of this function _should not_ set their descriptors
    /// OWNed, even if it's a receive descriptor and you feel like you ought to.
    /// Doing this makes the returned `Self` into a potential aliasing problem
    /// if it gets moved into a position where the hardware can see it while we
    /// still have a reference to it. Let `Ring` set it to owned safely by
    /// setting `INITIALLY_OWNED_BY_HW`.
    fn initial_state(buf: *mut u8, len: usize) -> Self;

    /// Checks the OWN bit on the descriptor to see if we can use it.
    fn is_owned_by_hw(&self) -> bool;

    /// Marks this descriptor as owned by the hardware, by setting the OWN bit,
    /// and simultaneously gives up further access to the descriptor.
    ///
    /// Note: self is `'static` because that gives self move semantics, i.e. the
    /// caller cannot use `self` again. If it were `&mut self` then `self` could
    /// be a reborrow, and the caller could retain access.
    fn set_owned_by_hw(&'static mut self);

    /// Flag controlling whether a newly created ring of this type is
    /// immediately set to hardware ownership.
    const INITIALLY_OWNED_BY_HW: bool;
}

/// Signal returned from ring operations to allow them to back out changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Commit {
    /// The operation did not succeed and resources should be released, rather
    /// than handed to the hardware.
    No,
    /// The operation succeeded.
    Yes,
}

/// A ring of descriptors, and a matching ring of buffers.
///
/// A `Ring` gets constructed from a static array of descriptors, and a matching
/// static array of buffers. It takes ownership of both.
///
/// At any given time, a range of descriptors within the ring is owned by the
/// hardware -- that range may be empty. An index maintained by the ring marks
/// _one_ of the ends of this range; the other is implicitly tracked using the
/// OWN bits.
///
/// ```text
///   +------------------------------------------------+
///   |  avail   |    OWNED  |  avail                  |
///   +------------------------------------------------+
///      next, --^            ^-- next, when tx
///      when rx
/// ```
///
/// The hardware moves through descriptors -- taking new ones and returning old
/// ones -- in one direction. The descriptor at index `next` is the next one
/// software will attempt to use.
///
/// # Transmit vs receive
///
/// The way software interacts with the descriptor ring depends on the direction
/// of data flow. While this module is shared between both TX and RX, it's
/// easier to understand the generic mechanism in terms of its two concrete
/// applications.
///
/// In the transmit case,
///
/// - All descriptors are initially owned by software. This is the steady state.
/// - When software wishes to transmit, it fills in the descriptor at `next` and
///   marks it as OWNed by the hardware, advancing `next`.
/// - If the ring fills up, software will find the `next` descriptor still OWNed
///   when it goes to transmit, and will have to try again later.
///
/// In the receive case,
///
/// - All descriptors are initially owned by hardware. This is the steady state.
/// - When hardware finishes writing out a packet, it fills in the descriptor at
///   `next` and clears its OWN bit.
/// - Software can inspect the descriptor at `next` and set it back to OWNed
///   when done with its data, advancing `next`.
/// - Software can tell the ring is empty because the descriptor at `next` is
///   OWNed by the hardware.
pub struct Ring<T, const MTU: usize> {
    /// Pointer to base of the descriptor ring. This is a pointer, rather than a
    /// `&'static mut`, because we own the descriptors but loan them to the DMA
    /// hardware, which would violate the aliasing rules of a `&mut`.
    descriptors: *mut T,
    /// Pointer to base of the frame buffer ring. This is a pointer, rather than
    /// a `&'static mut`, because we own the descriptors but loan them to the
    /// DMA hardware, which would violate the aliasing rules of a `&mut`.
    buffers: *mut MaybeUninit<[u8; MTU]>,
    /// Number of entries in the rings.
    len: usize,
    /// Index of the next buffer that the driver will access.
    ///
    /// Invariant: always `<= len`.
    next: usize,
}

impl<T: Descriptor, const MTU: usize> Ring<T, MTU> {
    /// Creates a descriptor ring that owns the given sections of memory.
    pub fn new(
        descriptors: &'static mut [MaybeUninit<T>],
        buffers: &'static mut [MaybeUninit<[u8; MTU]>],
    ) -> Self {
        assert_eq!(descriptors.len(), buffers.len());

        for (desc, buf) in descriptors.iter_mut().zip(buffers.iter_mut()) {
            unsafe {
                let first_elt = buf.as_mut_ptr() as *mut u8;
                desc.as_mut_ptr().write(T::initial_state(first_elt, MTU));
                if T::INITIALLY_OWNED_BY_HW {
                    // We have to materialize a reference to use the normal
                    // set_owned_by_hw codepath, but that's okay, because since
                    // we're building the ring still, the descriptors are not
                    // yet visible to the hardware, by definition.
                    (*desc.as_mut_ptr()).set_owned_by_hw();
                }
            }
        }

        Self {
            descriptors: descriptors.as_mut_ptr() as *mut T,
            buffers: buffers.as_mut_ptr(),
            len: descriptors.len(),
            next: 0,
        }
    }

    /// Obtains the next buffer, if it is not still owned by the hardware.
    ///
    /// # If the buffer is free
    ///
    /// If the descriptor is not owned by the hardware, calls `body` with
    /// exclusive references to both the descriptor and the buffer. `body` is
    /// expected to either fill out the descriptor/buffer and return
    /// `Commit::Yes`, at which point the descriptor will be passed to the
    /// hardware, or return `Commit::No`, in which case it will not. In the
    /// latter case, calling `with_next_buffer` again will get the same buffer.
    ///
    /// In either case, the `Commit` result from `body` is returned inside
    /// `Some`. If it is `Commit::Yes`, this function will take care of setting
    /// the descriptor's OWN bit after doing appropriate barrier operations;
    /// please do not set the OWN bit in `body`.
    ///
    /// # If the buffer is in use
    ///
    /// If the next buffer is owned by the hardware, `body` isn't called, and
    /// the result is `None`.
    ///
    /// # About that `MaybeUninit`
    ///
    /// If we hand a buffer to `body`, it's in the form of a `&mut
    /// MaybeUninit<[u8; MTU]>`. This is a deliberately conservative choice, and
    /// is intended to make it more difficult to accidentally send either
    /// uninitialized or stale data as part of an outgoing packet, or interpret
    /// uninitialized or stale data as incoming. By acting like the contents of
    /// each buffer are totally undefined at the start of each use, we push
    /// responsibility for accessing the memory safety up one level, to the
    /// tx/rx rings, which each have their own strategies for handling it.
    ///
    /// # Safety
    ///
    /// This operation is unsafe for _exactly one reason_: if this ring is being
    /// used by the hardware, _and_ the function you pass as `body` sets the OWN
    /// bit of the descriptor it receives, then we are data-racing the DMA
    /// controller into UB-land.
    ///
    /// So: to use this safely, you must provide a `body` that does not set the
    /// OWN flag of the descriptor. That's it! Have fun.
    pub unsafe fn with_next_buffer(
        &mut self,
        body: impl FnOnce(&mut T, &mut MaybeUninit<[u8; MTU]>) -> Commit,
    ) -> Option<Commit> {
        // Index into the arrays using pointer arithmetic.
        let dp = self.descriptors.wrapping_add(self.next);
        let bp = self.buffers.wrapping_add(self.next);
        // We might be racing the hardware for this descriptor, so initially we
        // can only create a shared reference. Explicitly limit its scope with a
        // block to ensure that it does not overlap the &mut below.
        {
            let d = unsafe { &*dp };
            // TODO this is where we'd invalidate the cache lines containing the
            // descriptor.
            if d.is_owned_by_hw() {
                return None;
            }
        }

        // The hardware is not using this descriptor, it is safe for us to
        // reference it. Since (1) we have a &mut to self and (2) the hardware
        // never _sets_ this bit, we don't have a race between the check above
        // and the use below.
        let d = unsafe { &mut *dp };
        let result = body(d, unsafe { &mut *bp });

        if result == Commit::Yes {
            // Make sure that any descriptor writes performed in `body` have
            // completed before we make further writes to set the OWN bit.
            membar::store_store_barrier();

            d.set_owned_by_hw();

            // TODO this is where we'd flush the descriptor to main memory

            self.next += 1;
            if self.next == self.len {
                self.next = 0;
            }
        }

        Some(result)
    }

    /// Pointer to the lowest address of the descriptor ring.
    ///
    /// This is typically the value that must be provided to hardware, so that
    /// it knows where to hunt for descriptors.
    pub fn base_ptr(&self) -> *const T {
        self.descriptors
    }

    /// Pointer to the "next" descriptor (as described in the docs on `Ring`).
    ///
    /// This is intended to be provided to a hardware "doorbell" mechanism to
    /// notify it of descriptor updates up to a certain address.
    pub fn next_ptr(&self) -> *const T {
        self.descriptors.wrapping_add(self.next)
    }

    /// Returns the number of descriptors/buffers in this ring.
    pub fn len(&self) -> usize {
        self.len
    }
}
