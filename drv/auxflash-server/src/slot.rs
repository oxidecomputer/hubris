// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::{hint::assert_unchecked, ops::Range};
use drv_auxflash_api::{
    AuxFlashError, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES, SLOT_COUNT, SLOT_SIZE,
};

/// A verified slot number.
///
/// Slots always come in pairs, `N` and `N + 1` where `N` is even. The two
/// are used as redundant pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub(crate) struct Slot(u32);

impl Slot {
    /// Create a Slot value from a slot index without checking its validity.
    ///
    /// # Safety
    ///
    /// The slot index must be less than [`SLOT_COUNT`], or in other words must
    /// be a valid slot index on the flash.
    ///
    /// [`SLOT_COUNT`]: SLOT_COUNT
    #[inline]
    pub(crate) const unsafe fn new_unchecked(slot: u32) -> Self {
        debug_assert!(slot < SLOT_COUNT);
        Self(slot)
    }

    /// Get the slot index.
    #[inline]
    pub(crate) const fn value(self) -> u32 {
        self.0
    }

    /// Get the slot index as usize.
    const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Get the slot start memory address.
    #[inline]
    pub(crate) const fn memory_start(self) -> usize {
        // SAFETY: Slot is checked to never overflow memory.
        unsafe { self.as_usize().unchecked_mul(SLOT_SIZE) }
    }

    /// Get the slot end memory address.
    #[inline]
    pub(crate) const fn memory_end(self) -> usize {
        // SAFETY: Slot is checked to never overflow memory.
        unsafe {
            self.as_usize()
                .unchecked_mul(SLOT_SIZE)
                .unchecked_add(SLOT_SIZE)
        }
    }

    /// Get the redundant pair of this slot.
    #[inline]
    pub(crate) const fn get_redundant_slot(self) -> Self {
        Self(self.0 ^ 1)
    }

    /// Get the memory address of an offset within this slot.
    #[inline]
    pub(crate) const fn with_offset(self, offset: SlotOffset) -> usize {
        // SAFETY: checked slot and offset
        unsafe { self.memory_start().unchecked_add(offset.value()) }
    }

    /// Get the memory address of an offset within a slot.
    #[inline]
    pub(crate) fn address_with_offset(
        slot: u32,
        offset: u32,
    ) -> Result<usize, AuxFlashError> {
        let slot = Self::try_from(slot)?;
        let offset = SlotOffset::try_from(offset)?;
        Ok(slot.with_offset(offset))
    }

    /// Get the memory range of this slot.
    #[inline]
    pub(crate) const fn memory_range(self) -> Range<usize> {
        let mem_start = self.memory_start();
        let mem_end = self.memory_end();
        debug_assert!((mem_end - mem_start) == SLOT_SIZE);
        const {
            assert!(
                SLOT_SIZE.is_multiple_of(SECTOR_SIZE_BYTES),
                "Slot must be a multiple of sectors"
            );
        };
        mem_start..mem_end
    }

    /// Iterate over the slot memory range in sectors. The iterator returns the
    /// start address of each sector.
    #[inline]
    pub(crate) fn as_sectors(self) -> impl Iterator<Item = usize> {
        let range = self.memory_range();
        debug_assert!(range.len() == SLOT_SIZE);
        const { assert!(SLOT_SIZE.is_multiple_of(SECTOR_SIZE_BYTES)) };
        SlotSectorIterator {
            next: range.start,
            end: range.end,
        }
    }

    /// Iterate over a half-open range of offsets in the slot as chunks. The
    /// last chunk is smaller than the chunk size if the range is not a multiple
    /// of the chunk size.
    #[inline]
    pub(crate) const fn as_chunks<const CHUNK_SIZE: usize>(
        self,
        range: Range<SlotOffset>,
    ) -> impl Iterator<Item = Range<usize>> {
        let next = self.with_offset(range.start);
        let end = self.with_offset(range.end);
        SlotChunkIterator::<CHUNK_SIZE> { next, end }
    }
}

impl TryFrom<u32> for Slot {
    type Error = AuxFlashError;

    fn try_from(value: u32) -> Result<Self, AuxFlashError> {
        if value >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot);
        }
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }
        Ok(Self(value))
    }
}

/// Memory offset within a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub(crate) struct SlotOffset(usize);

impl SlotOffset {
    /// Zero offset.
    pub(crate) const ZERO: Self = Self(0);

    /// Create a new SlotOffset from an offset without checking its value.
    ///
    /// # Safety
    ///
    /// The value must be less or equal to SLOT_SIZE.
    pub(crate) const unsafe fn new_unchecked(value: usize) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> usize {
        self.0
    }

    pub(crate) fn add(self, value: usize) -> Result<SlotOffset, AuxFlashError> {
        self.0
            .checked_add(value)
            .ok_or(AuxFlashError::AddressOverflow)
            .and_then(Self::try_from)
    }
}

impl TryFrom<u32> for SlotOffset {
    type Error = AuxFlashError;

    fn try_from(value: u32) -> Result<Self, AuxFlashError> {
        Self::try_from(value as usize)
    }
}

impl TryFrom<usize> for SlotOffset {
    type Error = AuxFlashError;

    fn try_from(value: usize) -> Result<Self, AuxFlashError> {
        if value > SLOT_SIZE {
            return Err(AuxFlashError::AddressOverflow);
        }
        Ok(Self(value))
    }
}

impl TryFrom<u64> for SlotOffset {
    type Error = AuxFlashError;

    fn try_from(value: u64) -> Result<Self, AuxFlashError> {
        if value > SLOT_SIZE as u64 {
            return Err(AuxFlashError::AddressOverflow);
        }
        Ok(Self(value as usize))
    }
}

/// Memory offset within a slot that is guaranteed to be aligned to a page.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub(crate) struct SlotPageOffset(SlotOffset);

impl SlotPageOffset {
    pub(crate) fn add(self, value: usize) -> Result<SlotOffset, AuxFlashError> {
        self.0.add(value)
    }
}

impl TryFrom<u32> for SlotPageOffset {
    type Error = AuxFlashError;

    fn try_from(value: u32) -> Result<Self, AuxFlashError> {
        Self::try_from(value as usize)
    }
}

impl TryFrom<usize> for SlotPageOffset {
    type Error = AuxFlashError;

    fn try_from(value: usize) -> Result<Self, AuxFlashError> {
        if !value.is_multiple_of(PAGE_SIZE_BYTES) {
            return Err(AuxFlashError::UnalignedAddress.into());
        }
        Ok(Self(SlotOffset::try_from(value)?))
    }
}

/// Iterator over sectors of a slot.
#[derive(Debug)]
pub(crate) struct SlotSectorIterator {
    next: usize,
    end: usize,
}

impl Iterator for SlotSectorIterator {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next < self.end {
            let value = self.next;
            // SAFETY: Slot is an exact multiple of sectors; this cannot
            // overflow.
            self.next = unsafe { value.unchecked_add(SECTOR_SIZE_BYTES) };
            Some(value)
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let size = self.end.saturating_sub(self.next);
        (size, Some(size))
    }
}

impl ExactSizeIterator for SlotSectorIterator {}

// Iterator over chunks in a slot.
#[derive(Debug)]
pub(crate) struct SlotChunkIterator<const CHUNK_SIZE: usize> {
    next: usize,
    end: usize,
}

impl<const CHUNK_SIZE: usize> Iterator for SlotChunkIterator<CHUNK_SIZE> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next < self.end {
            let start = self.next;
            let end = start
                .checked_add(CHUNK_SIZE)
                .map_or(self.end, |end| end.min(self.end));
            self.next = end;
            let range = Range::from(start..end);
            // SAFETY: by construction.
            unsafe {
                assert_unchecked(range.len() <= CHUNK_SIZE);
            };
            Some(range)
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let size = self.end.saturating_sub(self.next);
        (size, Some(size))
    }
}

impl<const CHUNK_SIZE: usize> ExactSizeIterator
    for SlotChunkIterator<CHUNK_SIZE>
{
}
