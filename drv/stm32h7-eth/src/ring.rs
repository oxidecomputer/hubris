// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! DMA descriptor rings and data buffers.
//!
//! While this module is specific to the DMA descriptor format used by the
//! Synopsys Ethernet MAC in the STM32H7, it does _not_ depend on the hardware.
//! This module just moves memory around very carefully.
//!
//! Note that the APIs on this module are entirely safe. This is deliberate. The
//! only unsafe act that a user of this module should expect to perform is
//! setting up the `static mut` data buffers required to call `new` on the
//! respective ring types.

// The ring APIs in general do not need to know if something is empty.
#![allow(clippy::len_without_is_empty)]

use core::cell::{Cell, UnsafeCell};
use core::sync::atomic::{AtomicU32, Ordering};

/// This can be used in an array initializer, while `AtomicU32::new(0)` cannot.
/// Believe it!
#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO: AtomicU32 = AtomicU32::new(0);

/// Similarly, we have to make this intermediate array to store an array of
/// arrays of AtomicZero
#[cfg(feature = "vlan")]
#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO_FOUR: [AtomicU32; 4] = [ATOMIC_ZERO; 4];

/// Size of buffer used with the Ethernet DMA. This can be changed but must
/// remain under 64kiB -- the driver initialization code refers to this constant
/// when setting up the controller.
pub const BUFSZ: usize = 1536;

/// Opaque (to you) type alias for an Ethernet packet buffer. To use this module
/// you need to create a static array of these somehow and provide it to `new`.
pub struct Buffer(UnsafeCell<[u8; BUFSZ]>);

/// We are careful to use `Buffer` in thread-safe ways and need it to be `Sync`
/// so that it can be placed in a `static` by our users.
unsafe impl Sync for Buffer {}

impl Buffer {
    /// Creates a zero-initialized buffer.
    pub const fn new() -> Self {
        Self(UnsafeCell::new([0; BUFSZ]))
    }
}

/// Transmit descriptor record.
///
/// This is deliberately opaque to viewers outside this module, so that we can
/// carefully control accesses to its contents.
///
/// When configured in VLAN mode, we write _two_ descriptors (each 4 bytes):
/// - the configuration descriptor, which sets the VLAN for subsequent packets
/// - the actual packet transmit descriptor
#[repr(transparent)]
pub struct TxDesc {
    /// Transmit descriptor
    #[cfg(not(feature = "vlan"))]
    tdes: [AtomicU32; 4],

    /// Context and transmit descriptors, packed together
    #[cfg(feature = "vlan")]
    tdes: [[AtomicU32; 4]; 2],
}

impl TxDesc {
    pub const fn new() -> Self {
        Self {
            #[cfg(not(feature = "vlan"))]
            tdes: [ATOMIC_ZERO; 4],
            #[cfg(feature = "vlan")]
            tdes: [ATOMIC_ZERO_FOUR; 2],
        }
    }
}

/// Index of OWN bit indicating that a descriptor is in use by the hardware.
const TDES3_OWN_BIT: u32 = 31;
/// Index of First Descriptor bit, indicating that a descriptor is the start of
/// a new packet. We always set this and Last Descriptor, below.
const TDES3_FD_BIT: u32 = 29;
/// Index of Last Descriptor bit, indicating that a descriptor is the end of a
/// packet. We always set this and First Descriptor, above.
const TDES3_LD_BIT: u32 = 28;

/// Index of Checksum Insertion Control field.
const TDES3_CIC_BIT: u32 = 16;
/// CIC value for enabling all checksum offloading.
const TDES3_CIC_CHECKSUMS_ENABLED: u32 = 0b11;

// TDES bits which are only used in VLAN code, gated to avoid compiler warnings
cfg_if::cfg_if! {
    if #[cfg(feature = "vlan")] {
        /// Index of CTXT bit indicating that this is a context descriptor.
        const TDES3_CTXT_BIT: u32 = 30;
        /// Index of VLAN Tag Valid bit in a Tx Context descriptor.
        const TDES3_VLTV_BIT: u32 = 16;
        /// Index of VLAN Tag Insertion or Replacement field.
        const TDES2_VTIR_BIT: u32 = 14;
        /// VTIR value for inserting a VLAN tag.
        const TDES2_VTIR_INSERT: u32 = 0b10;
    }
}

/// Control block for a ring of `TxDesc` records and associated `Buffer`s.
pub struct TxRing {
    /// The descriptor ring storage.
    storage: &'static [TxDesc],
    /// The buffers we're sharing with the hardware.
    buffers: &'static [Buffer],
    /// Index of the element within `storage` where we'll try to deposit the
    /// next transmitted packet. This must be in the range `0..storage.len()` at
    /// all times.
    next: Cell<usize>,
}

impl TxRing {
    /// Creates a new TX DMA ring out of `storage` and `buffers`.
    ///
    /// Note that, because `&'static mut` is not `Copy` and cannot be a reborrow
    /// (because `'static` is the longest lifetime), you lose access to both
    /// slices upon calling this. There is no (safe) way to get the pieces back
    /// out. This is deliberate.
    ///
    /// # Panics
    ///
    /// If `storage` and `buffers` are not the same length.
    pub fn new(
        storage: &'static mut [TxDesc],
        buffers: &'static mut [Buffer],
    ) -> Self {
        assert_eq!(storage.len(), buffers.len());
        // Drop mutability. We needed the caller to prove exclusive ownership,
        // but we don't actually need &mut. We assume that both areas of memory
        // are shared with the DMA controller from this point forward.
        let (storage, buffers) = (&*storage, &*buffers);
        // Initialize all TxDesc records to a known state, and in particular,
        // ensure that they're owned by us (not the hardware).
        for desc in storage {
            #[cfg(not(feature = "vlan"))]
            desc.tdes[3].store(0, Ordering::Release);

            #[cfg(feature = "vlan")]
            {
                desc.tdes[0][3].store(0, Ordering::Release);
                desc.tdes[1][3].store(0, Ordering::Release);
            }
        }
        Self {
            storage,
            buffers,
            next: Cell::new(0),
        }
    }

    /// Returns the base pointer of the `TxDesc` ring. This needs to be loaded
    /// into the DMA controller so it knows where to look for descriptors.
    pub fn base_ptr(&self) -> *const TxDesc {
        self.storage.as_ptr()
    }

    /// Returns a pointer to the byte just past the end of the `TxDesc` ring.
    /// This too gets loaded into the DMA controller, so that it knows what
    /// section of the ring is initialized and can be read. (The answer is "all
    /// of it.")
    pub fn tail_ptr(&self) -> *const TxDesc {
        self.storage.as_ptr_range().end
    }
}

#[cfg(not(feature = "vlan"))]
impl TxRing {
    /// Returns the count of entries in the descriptor ring / buffers in the
    /// pool.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_next_free(&self) -> bool {
        let d = &self.storage[self.next.get()];
        // Check whether the hardware has released this.
        let tdes3 = d.tdes[3].load(Ordering::Acquire);
        let own = tdes3 & (1 << TDES3_OWN_BIT) != 0;
        !own
    }

    /// Attempts to grab the next unused TX buffer in the ring and deposit a
    /// packet into it.
    ///
    /// If the next buffer in the ring is not holding a pending packet, this
    /// will borrow it and call `body` with its address. `body` returns the
    /// number of bytes in the packet; this routine will then set up a pending
    /// transmit descriptor for that prefix of the buffer, and return
    /// `Some(len)`.
    ///
    /// If the next buffer in the ring is pending, that means we have run out of
    /// TX ring slots. In that case, this will return `None` without invoking
    /// `body`.
    ///
    /// # Panics
    ///
    /// If a buffer is available, and we call `body`, and `body` returns a valid
    /// length larger than `BUFSZ`. Because that's obviously wrong.
    pub fn try_with_next<R>(
        &self,
        len: usize,
        body: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let d = &self.storage[self.next.get()];
        // Check whether the hardware has released this.
        let tdes3 = d.tdes[3].load(Ordering::Acquire);
        let own = tdes3 & (1 << TDES3_OWN_BIT) != 0;
        if own {
            None
        } else {
            // Descriptor is free. Since we keep the descriptors paired with
            // their corresponding buffers, and only lend out the buffers
            // temporarily (like we're about to do), this means the buffer is
            // also free.
            let buffer = self.buffers[self.next.get()].0.get();
            // Safety: we're dereferencing a raw *mut to get a &mut into the
            // buffer. We must ensure that the pointer is valid (which we can
            // tell trivially from the fact that we got it from an UnsafeCell)
            // and that there is no aliasing. We can say there's no aliasing
            // because the descriptor is free (above), so we're about to produce
            // the sole reference to it, which won't outlive this block.
            let buffer = unsafe { &mut *buffer };
            let buffer = &mut buffer[..len];

            let result = body(buffer);

            // Program the descriptor to represent the packet. We program
            // carefully to ensure that the memory accesses happen in the right
            // order: the entire descriptor must be written before the OWN bit
            // is set in TDES3 using a RELEASE store.
            d.tdes[0].store(buffer.as_ptr() as u32, Ordering::Relaxed);
            d.tdes[1].store(0, Ordering::Relaxed);
            d.tdes[2].store(len as u32, Ordering::Relaxed);
            let tdes3 = 1 << TDES3_OWN_BIT
                | 1 << TDES3_FD_BIT
                | 1 << TDES3_LD_BIT
                | TDES3_CIC_CHECKSUMS_ENABLED << TDES3_CIC_BIT
                | len as u32;
            d.tdes[3].store(tdes3, Ordering::Release); // <-- release

            self.next.set(if self.next.get() + 1 == self.storage.len() {
                0
            } else {
                self.next.get() + 1
            });

            Some(result)
        }
    }
}

#[cfg(feature = "vlan")]
impl TxRing {
    /// Returns the count of entries in the descriptor ring / buffers in the
    /// pool.
    pub fn len(&self) -> usize {
        self.storage.len() * 2 // Two descriptors per slot!
    }

    pub fn is_next_free(&self) -> bool {
        let d = &self.storage[self.next.get()];
        // Check whether the hardware has released both the context descriptor
        // and the following transmit descriptor.
        let tdes3 = d.tdes[0][3].load(Ordering::Acquire);
        let own1 = tdes3 & (1 << TDES3_OWN_BIT) != 0;

        let tdes3 = d.tdes[1][3].load(Ordering::Relaxed);
        let own2 = tdes3 & (1 << TDES3_OWN_BIT) != 0;

        !(own1 || own2)
    }

    pub fn vlan_try_with_next<R>(
        &self,
        len: usize,
        vid: u16,
        body: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let d = &self.storage[self.next.get()];
        // Check whether the hardware has released both the Context and Tx
        // descriptors.
        let tdes3 = d.tdes[0][3].load(Ordering::Acquire);
        let own1 = tdes3 & (1 << TDES3_OWN_BIT) != 0;

        let tdes3 = d.tdes[1][3].load(Ordering::Acquire);
        let own2 = tdes3 & (1 << TDES3_OWN_BIT) != 0;
        if own1 || own2 {
            None
        } else {
            // Descriptor is free. Since we keep the descriptors paired with
            // their corresponding buffers, and only lend out the buffers
            // temporarily (like we're about to do), this means the buffer is
            // also free.
            let buffer = self.buffers[self.next.get()].0.get();
            // Safety: we're dereferencing a raw *mut to get a &mut into the
            // buffer. We must ensure that the pointer is valid (which we can
            // tell trivially from the fact that we got it from an UnsafeCell)
            // and that there is no aliasing. We can say there's no aliasing
            // because the descriptor is free (above), so we're about to produce
            // the sole reference to it, which won't outlive this block.
            let buffer = unsafe { &mut *buffer };
            let buffer = &mut buffer[..len];

            let result = body(buffer);

            // Program the context descriptor to configure the VLAN tag. We
            // program carefully to ensure that the memory accesses happen
            // in the right order: the entire descriptor must be written before
            // the OWN bit is set in TDES3 using a RELEASE store.
            let tdes3 = 1 << TDES3_OWN_BIT
                | 1 << TDES3_CTXT_BIT
                | 1 << TDES3_VLTV_BIT
                | u32::from(vid);
            d.tdes[0][3].store(tdes3, Ordering::Release); // <-- release

            // Program the tx descriptor to represent the packet, using the
            // same strategy as above for memory access ordering.
            d.tdes[1][0].store(buffer.as_ptr() as u32, Ordering::Relaxed);
            d.tdes[1][1].store(0, Ordering::Relaxed);
            let tdes2 = TDES2_VTIR_INSERT << TDES2_VTIR_BIT | len as u32;
            d.tdes[1][2].store(tdes2, Ordering::Relaxed);
            let tdes3 = 1 << TDES3_OWN_BIT
                | 1 << TDES3_FD_BIT
                | 1 << TDES3_LD_BIT
                | TDES3_CIC_CHECKSUMS_ENABLED << TDES3_CIC_BIT
                | len as u32;
            d.tdes[1][3].store(tdes3, Ordering::Release); // <-- release

            self.next.set(if self.next.get() + 1 == self.storage.len() {
                0
            } else {
                self.next.get() + 1
            });

            Some(result)
        }
    }
}

/// Receive descriptor record.
///
/// This is deliberately opaque to viewers outside this module, so that we can
/// carefully control accesses to its contents.
#[repr(transparent)]
pub struct RxDesc {
    rdes: [AtomicU32; 4],
}

impl RxDesc {
    pub const fn new() -> Self {
        Self {
            rdes: [ATOMIC_ZERO; 4],
        }
    }
}

/// Index of OWN bit indicating that a descriptor is in use by the hardware.
const RDES3_OWN_BIT: u32 = 31;
/// Index of Error Summary bit, which rolls up all the other error bits.
const RDES3_ES_BIT: u32 = 15;
/// Index of Interrupt On Completion bit, indicating that a we want to be
/// notified when a packet arrives in this descriptor slot (we always request
/// this).
const RDES3_IOC_BIT: u32 = 30;
/// Index of First Descriptor bit, indicating that a descriptor is the start of
/// a new packet. We always expect this and Last Descriptor, below.
const RDES3_FD_BIT: u32 = 29;
/// Index of Last Descriptor bit, indicating that a descriptor is the end of a
/// packet. We always expect this and First Descriptor, above.
const RDES3_LD_BIT: u32 = 28;
/// Index of Buffer 1 Valid bit, indicating that we have furnished a valid
/// pointer for buffer 1 in this descriptor.
const RDES3_BUF1_VALID_BIT: u32 = 24;
/// Mask for the Packet Length portion of RDES3.
const RDES3_PL_MASK: u32 = (1 << 15) - 1;

// RDES bits which are only used in VLAN code, gated to avoid compiler warnings
cfg_if::cfg_if! {
    if #[cfg(feature = "vlan")] {
        /// Amount to shift RDES0 to read out the VLAN ID as a `u16`
        const RDES0_OUTER_VID_BIT: u32 = 0;
        /// Index of Receive Status RDES0 Valid bit, indicating that RDES0 is
        /// valid and has been written by the DMA.
        const RDES3_RS0V_BIT: u32 = 25;
    }
}

/// Control block for a ring of `RxDesc` records and associated `Buffer`s.
pub struct RxRing {
    /// The descriptor ring storage.
    storage: &'static [RxDesc],
    /// The buffers we're sharing with the hardware.
    buffers: &'static [Buffer],
    /// Index of the element within `storage` where we'll look for the next
    /// received packet. This must be in the range `0..storage.len()` at all
    /// times.
    next: Cell<usize>,
}

impl RxRing {
    /// Creates a new RX DMA ring out of `storage` and `buffers`.
    ///
    /// Note that, because `&'static mut` is not `Copy` and cannot be a reborrow
    /// (because `'static` is the longest lifetime), you lose access to both
    /// slices upon calling this. There is no (safe) way to get the pieces back
    /// out. This is deliberate.
    ///
    /// # Panics
    ///
    /// If `storage` and `buffers` are not the same length.
    pub fn new(
        storage: &'static mut [RxDesc],
        buffers: &'static mut [Buffer],
    ) -> Self {
        assert_eq!(storage.len(), buffers.len());

        // Give up &mut access to the buffers. We needed the caller to give us
        // &mut to prove they had, and now we have, exclusive access -- but
        // we're going to share it.
        let (storage, buffers) = (&*storage, &*buffers);
        // Program all descriptors with the matching buffer address and mark
        // them as available to hardware.
        for (desc, buf) in storage.iter().zip(buffers) {
            Self::set_descriptor(desc, buf.0.get());
        }

        Self {
            storage,
            buffers,
            next: Cell::new(0),
        }
    }

    /// Returns the base pointer of the `RxDesc` ring. This needs to be loaded
    /// into the DMA controller so it knows where to look for descriptors.
    pub fn base_ptr(&self) -> *const RxDesc {
        self.storage.as_ptr()
    }

    /// Returns a pointer to the byte just past the end of the `RxDesc` ring.
    /// This too gets loaded into the DMA controller, so that it knows what
    /// section of the ring is initialized and can be read. (The answer is "all
    /// of it.")
    pub fn tail_ptr(&self) -> *const RxDesc {
        self.storage.as_ptr_range().end
    }

    /// Returns the count of entries in the descriptor ring / buffers in the
    /// pool.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Programs the words in `d` to prepare to receive into `buffer` and sets
    /// `d` accessible to hardware. The final write to make it accessible is
    /// performed with Release ordering to get a barrier.
    fn set_descriptor(d: &RxDesc, buffer: *mut [u8; BUFSZ]) {
        d.rdes[0].store(buffer as u32, Ordering::Relaxed);
        d.rdes[1].store(0, Ordering::Relaxed);
        d.rdes[2].store(0, Ordering::Release);
        // See hubris#750 for why we need Ordering::Release and this delay
        cortex_m::asm::delay(16);
        let rdes3 =
            1 << RDES3_OWN_BIT | 1 << RDES3_IOC_BIT | 1 << RDES3_BUF1_VALID_BIT;
        d.rdes[3].store(rdes3, Ordering::Release); // <-- release
    }
}

#[cfg(not(feature = "vlan"))]
impl RxRing {
    /// Checks whether the next Rx slot is available to read (from userland).
    /// Drops invalid packets from the Rx ring, to prevent them from clogging
    /// things up. Returns a tuple `(next_free, any_dropped)`; if packets have
    /// been dropped, the caller should poke the DMA registers to inform them.
    pub fn is_next_free(&self) -> (bool, bool) {
        let mut any_dropped = false;
        loop {
            let d = &self.storage[self.next.get()];
            // Check whether the hardware has released this.
            let rdes3 = d.rdes[3].load(Ordering::Acquire);

            // If the hardware still owns this descriptor, then return right
            // away (and wait for the hardware to do more stuff).
            if rdes3 & (1 << RDES3_OWN_BIT) != 0 {
                return (false, any_dropped);
            }

            // What sort of descriptor is this?
            let errors = rdes3 & (1 << RDES3_ES_BIT) != 0;
            let first_and_last = rdes3
                & ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT))
                == ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT));

            // If this descriptor is error-free and represents a complete
            // packet, then return true so that the netstack loads it
            if !errors && first_and_last {
                return (true, any_dropped);
            }

            // Otherwise, drop the packet by bumping our index
            self.next.set(if self.next.get() + 1 == self.storage.len() {
                0
            } else {
                self.next.get() + 1
            });
            any_dropped = true;
        }
    }

    /// Grabs the next filled-out RX buffer in the ring and shows it to you
    ///
    /// The next buffer in the ring should be holding a valid pending packet,
    /// as checked by `is_next_free`.  This will borrow it and call `body` with
    /// the valid prefix of the buffer, based on the received length. Once
    /// `body` returns, this routine restores the ring entry to empty so that
    /// it can be used to receive another packet.
    ///
    /// This should only be called from an `RxToken`, to ensure that it's only
    /// called when a packet is available.
    ///
    /// `body` is allowed to return a value, of some type `R`. If we
    /// successfully grab a packet and call `body`, we'll return its result.
    /// This may or may not prove useful.
    ///
    /// # Panics
    /// If this function is called when the next packet in the queue is
    /// (a) owned by the DMA hardware or
    /// (b) not valid (i.e. an error descriptor)
    /// this function will panic.
    ///
    /// This should never happen in correctly-written code, as this function
    /// should only be called from an `RxToken`, which is only constructed
    /// after confirming that the next packet is available and valid.
    pub fn with_next<R>(&self, body: impl FnOnce(&mut [u8]) -> R) -> R {
        let d = &self.storage[self.next.get()];
        // Check whether the hardware has released this.
        let rdes3 = d.rdes[3].load(Ordering::Acquire);
        let own = rdes3 & (1 << RDES3_OWN_BIT) != 0;
        assert!(!own);

        // Descriptor is free. Since we keep the descriptors paired with
        // their corresponding buffers, and only lend out the buffers
        // temporarily (like we're about to do), this means the buffer is
        // also free.

        // What sort of descriptor is this?
        let errors = rdes3 & (1 << RDES3_ES_BIT) != 0;
        let first_and_last = rdes3
            & ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT))
            == ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT));
        assert!(!errors);
        assert!(first_and_last);

        let buffer = self.buffers[self.next.get()].0.get();

        // Safety: because the descriptor is free we keep them
        // paired, we know the buffer is not aliased, so we're going
        // to dereference this raw pointer to produce the only
        // reference to its contents. And then discard it at the end
        // of this block.
        let buffer = unsafe { &mut *buffer };

        // Work out the valid slice of the packet.
        let packet_len = (rdes3 & RDES3_PL_MASK) as usize;

        // Pass in the initialized prefix of the packet.
        let result = (body)(&mut buffer[..packet_len]);

        // We need to consume this descriptor whether or not we handed
        // it off. Rewrite it as an empty rx descriptor:
        Self::set_descriptor(d, buffer);
        // At this point the descriptor is no longer free, the buffer is
        // potentially in use, and we must not access either.

        // Bump index forward.
        self.next.set(if self.next.get() + 1 == self.storage.len() {
            0
        } else {
            self.next.get() + 1
        });

        result
    }
}

#[cfg(feature = "vlan")]
impl RxRing {
    /// Check whether the next Rx slot is available to read (from userland)
    /// and has a matching VID. This function also quietly drops invalid
    /// packets from the Rx ring, to avoid blocking the queue.  It returns a
    /// tuple of `(next_free, any_dropped)`. If packets have been dropped, the
    /// caller should poke the DMA registers to inform them.
    pub fn vlan_is_next_free(
        &self,
        vid: u16,
        vid_range: core::ops::Range<u16>,
    ) -> (bool, bool) {
        let mut any_dropped = false;
        loop {
            let d = &self.storage[self.next.get()];

            // Check whether the hardware has released this.
            let rdes3 = d.rdes[3].load(Ordering::Acquire);
            // If the hardware still owns this descriptor, then return right
            // away (and wait for the hardware to do more stuff).
            if rdes3 & (1 << RDES3_OWN_BIT) != 0 {
                return (false, any_dropped);
            }

            // Check to see if this is an error descriptor.  If so (or if it's
            // not a complete packet, which shouldn't happen), then drop it.
            let errors = rdes3 & (1 << RDES3_ES_BIT) != 0;
            let first_and_last = rdes3
                & ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT))
                == ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT));
            let packet_okay = !errors && first_and_last;

            // If RDES0 is valid, then check for a VLAN match
            let rdes0_valid = rdes3 & (1 << RDES3_RS0V_BIT) != 0;
            if packet_okay && rdes0_valid {
                let rdes0 = d.rdes[0].load(Ordering::Relaxed);
                let this_vid = ((rdes0 >> RDES0_OUTER_VID_BIT) & 0xFFF) as u16;

                if this_vid == vid {
                    // If this matches our target VLAN, then we're good!
                    return (true, any_dropped);
                } else if vid_range.contains(&this_vid) {
                    // If this matches a _different_ valid VLAN, then return
                    // and trust that another instance will handle it.
                    return (false, any_dropped);
                }
            }

            // If we've gotten to this point in the code, the packet is
            //  (a) owned by userspace and
            //  (b) either has no VID or has an invalid VID
            // so we're going to drop it to avoid clogging the queue.

            // Rewrite to an empty rx descriptor (owned by DMA)
            let buffer = self.buffers[self.next.get()].0.get();
            Self::set_descriptor(d, buffer);

            // Bump index forward.
            self.next.set((self.next.get() + 1) % self.storage.len());

            any_dropped = true;
        }
    }
    /// Attempts to grab the next filled-out RX buffer in the ring that
    /// matches the given VLAN id `vid` and show it to you.
    ///
    /// This should only be called from an `RxToken`, after `vlan_can_recv`
    /// has confirmed that it's valid to receive a packet.
    ///
    /// Otherwise, this function will panic.
    pub fn vlan_with_next<R>(
        &self,
        vid: u16,
        body: impl FnOnce(&mut [u8]) -> R,
    ) -> R {
        let d = &self.storage[self.next.get()];

        // Check whether the hardware has released this.
        let rdes3 = d.rdes[3].load(Ordering::Acquire);
        let own = rdes3 & (1 << RDES3_OWN_BIT) != 0;
        assert!(!own);

        // Descriptor is free. Since we keep the descriptors paired with
        // their corresponding buffers, and only lend out the buffers
        // temporarily (like we're about to do), this means the buffer is
        // also free.

        // What sort of descriptor is this?
        let errors = rdes3 & (1 << RDES3_ES_BIT) != 0;
        let first_and_last = rdes3
            & ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT))
            == ((1 << RDES3_FD_BIT) | (1 << RDES3_LD_BIT));
        assert!(!errors);
        assert!(first_and_last);

        // If RDES0 is valid, then check for a VLAN match
        let rdes0_valid = rdes3 & (1 << RDES3_RS0V_BIT) != 0;
        assert!(rdes0_valid);

        let rdes0 = d.rdes[0].load(Ordering::Relaxed);
        let this_vid = ((rdes0 >> RDES0_OUTER_VID_BIT) & 0xFFF) as u16;
        assert_eq!(this_vid, vid);

        let buffer = self.buffers[self.next.get()].0.get();

        // Safety: because the descriptor is free we keep them
        // paired, we know the buffer is not aliased, so we're going
        // to dereference this raw pointer to produce the only
        // reference to its contents. And then discard it at the end
        // of this block.
        let buffer = unsafe { &mut *buffer };

        // Work out the valid slice of the packet.
        let packet_len = (rdes3 & RDES3_PL_MASK) as usize;

        // Pass in the initialized prefix of the packet.
        let retval = (body)(&mut buffer[..packet_len]);

        // We need to consume this descriptor whether or not we handed
        // it off. Rewrite it as an empty rx descriptor:
        Self::set_descriptor(d, buffer);
        // At this point the descriptor is no longer free, the buffer is
        // potentially in use, and we must not access either.

        // Bump index forward.
        self.next.set((self.next.get() + 1) % self.storage.len());

        retval
    }
}
