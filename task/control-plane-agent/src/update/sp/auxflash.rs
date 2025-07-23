// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use core::ops::Range;
use drv_auxflash_api::{
    AuxFlash, AuxFlashChecksum, AuxFlashError, PAGE_SIZE_BYTES,
    SECTOR_SIZE_BYTES, SLOT_COUNT, SLOT_SIZE,
};
use gateway_messages::{
    SpComponent, SpError, UpdateId, UpdateInProgressStatus,
    UpdatePreparationProgress, UpdatePreparationStatus, UpdateStatus,
};

const SECTORS_PER_SLOT: usize = SLOT_SIZE / SECTOR_SIZE_BYTES;

pub(super) enum State {
    ScanningForChck(ScanningForChck),
    ErasingSlot(ErasingSlot),
    FinishedErasingSlot(FinishedErasingSlot),
    AcceptingData(AcceptingData),
    Failed(AuxFlashError),
}

pub(super) enum ChckScanResult {
    FoundMatch(BorrowedUpdateBuffer),
    NewState(State),
}

pub(super) enum IngestDataResult {
    Done(BorrowedUpdateBuffer),
    NewState(State),
}

impl State {
    pub(super) fn new(
        task: &AuxFlash,
        mut buffer: BorrowedUpdateBuffer,
        chck: [u8; 32],
    ) -> Self {
        static_assertions::const_assert!(
            PAGE_SIZE_BYTES <= UpdateBuffer::MAX_CAPACITY
        );
        // We get `buffer` from `SpUpdate`; make sure it's the size we need, and
        // mark ourselves as owning it.
        buffer.reborrow(SpComponent::SP_AUX_FLASH, PAGE_SIZE_BYTES);
        let active_slot = task.get_active_slot().ok();

        Self::ScanningForChck(ScanningForChck {
            buffer,
            chck: AuxFlashChecksum(chck),
            active_slot,
            index: active_slot.unwrap_or(0),
            first_empty_even_slot: None,
        })
    }

    pub(super) fn is_preparing(&self) -> bool {
        match self {
            Self::ScanningForChck(_) | Self::ErasingSlot(_) => true,
            Self::FinishedErasingSlot(_)
            | Self::AcceptingData(_)
            | Self::Failed(_) => false,
        }
    }

    pub(super) fn status(&self, id: UpdateId, total_size: u32) -> UpdateStatus {
        match self {
            Self::ScanningForChck(scan) => {
                // For the purposes of our status, we only need to count how
                // many slots are remaining to scan; treat no active slot as
                // slot 0.
                let active_slot = scan.active_slot.unwrap_or(0);

                let slots_scanned = if scan.index < active_slot {
                    // Because we start `index` at `active_slot`, if `index` has
                    // wrapped around and is now less than `active_slot`, we've
                    // already scanned `active_slot..total_slots` and
                    // `0..index`.
                    (SLOT_COUNT - active_slot) + scan.index
                } else {
                    // Otherwise, we've only scanned
                    // `active_slot..index`.
                    scan.index - active_slot
                };

                UpdateStatus::Preparing(UpdatePreparationStatus {
                    id,
                    progress: Some(UpdatePreparationProgress {
                        current: slots_scanned,
                        total: SLOT_COUNT + SECTORS_PER_SLOT as u32,
                    }),
                })
            }
            Self::ErasingSlot(erase) => {
                UpdateStatus::Preparing(UpdatePreparationStatus {
                    id,
                    progress: Some(UpdatePreparationProgress {
                        current: SLOT_COUNT + erase.sectors_to_erase.start,
                        total: SLOT_COUNT + SECTORS_PER_SLOT as u32,
                    }),
                })
            }
            Self::FinishedErasingSlot(_) => {
                UpdateStatus::SpUpdateAuxFlashChckScan {
                    id,
                    found_match: false,
                    total_size,
                }
            }
            Self::AcceptingData(data) => {
                UpdateStatus::InProgress(UpdateInProgressStatus {
                    id,
                    bytes_received: data.next_write_offset
                        + data.buffer.len() as u32,
                    total_size,
                })
            }
            Self::Failed(err) => {
                UpdateStatus::Failed {
                    id,
                    // TODO These error codes conflict with `UpdateError` codes
                    // if we failed during the SP image update; is that okay?
                    // May be clear from context which code it is.
                    code: *err as u32,
                }
            }
        }
    }
}

pub(super) struct ScanningForChck {
    buffer: BorrowedUpdateBuffer,
    chck: AuxFlashChecksum,
    // The currently-active slot for our running image. This is the slot
    // index where we start scanning, since we typically expect to find a
    // match to our current aux flash image. If we have no active slot
    // (i.e., this is `None`), it is treated as 0.
    active_slot: Option<u32>,
    // Index of the slot to scan next. `active_slot - 1` (after wrapping
    // around at SLOT_COUNT) is the final slot to scan.
    index: u32,
    // While we're scanning, we record the first empty, even-numbered slot
    // we see. If we don't find a CHCK match, we'll pick this slot to
    // erase/write. If we don't find an empty, even slot, we'll the next
    // even slot above our current active slot.
    first_empty_even_slot: Option<u32>,
}

impl ScanningForChck {
    pub(super) fn continue_scanning(
        mut self,
        task: &AuxFlash,
    ) -> ChckScanResult {
        // Scan the slot at `index`.
        match task.read_slot_chck(self.index) {
            Ok(chck) => {
                // If this matches, we're done; transition to the
                // next state and return the buffer to our caller.
                if chck == self.chck {
                    return ChckScanResult::FoundMatch(self.buffer);
                }
            }
            // Error states that indicate missing or invalid data in a slot: If
            // we hit one of these errors in an even-numbered slot, we'll prefer
            // it as our target slot (unless we've already found one).
            Err(
                AuxFlashError::BadChckSize
                | AuxFlashError::MissingChck
                | AuxFlashError::MissingAuxi
                | AuxFlashError::MultipleChck
                | AuxFlashError::MultipleAuxi
                | AuxFlashError::ChckMismatch,
            ) => {
                // This error is expected for any empty slot; only
                // note its index if it's the first even empty slot
                // we've seen. (We'll use it as our target slot if
                // we don't find a matching CHCK in another.)
                if self.index.is_multiple_of(2)
                    && self.first_empty_even_slot.is_none()
                {
                    self.first_empty_even_slot = Some(self.index);
                }
            }
            Err(_) => {
                // What should we do with other errors? They indicate
                // some kind of problem with the auxflash itself, but
                // there's nothing we can do about it. For now, just
                // pretend it was a non-matching chck (i.e., skip).
            }
        }

        // The chck at index didn't match; advance to either the next
        // slot or the next state (if we've scanned all slots).
        self.index += 1;
        if self.index == SLOT_COUNT {
            self.index = 0;
        }

        // Have we wrapped back to where we started?
        if self.index == self.active_slot.unwrap_or(0) {
            // We need to pick a target slot: either take
            // `first_empty_even_slot`, if we found one, or round up
            // `active_slot` to the next even value.
            let target_slot = self.first_empty_even_slot.unwrap_or_else(|| {
                // Round up to next even number...
                let next_even = (self.index + 2) & !1;
                // and wrap back around to 0 if needed.
                next_even % SLOT_COUNT
            });
            ChckScanResult::NewState(State::ErasingSlot(ErasingSlot {
                buffer: self.buffer,
                chck: self.chck,
                slot: target_slot,
                sectors_to_erase: 0..SECTORS_PER_SLOT as u32,
            }))
        } else {
            ChckScanResult::NewState(State::ScanningForChck(self))
        }
    }
}

pub(super) struct ErasingSlot {
    buffer: BorrowedUpdateBuffer,
    chck: AuxFlashChecksum,
    slot: u32,
    sectors_to_erase: Range<u32>,
}

impl ErasingSlot {
    pub(super) fn continue_erasing(
        mut self,
        task: &AuxFlash,
    ) -> Result<State, AuxFlashError> {
        assert!(!Range::is_empty(&self.sectors_to_erase));
        let offset = self.sectors_to_erase.start * SECTOR_SIZE_BYTES as u32;
        task.slot_sector_erase(self.slot, offset)?;
        self.sectors_to_erase.start += 1;
        if Range::is_empty(&self.sectors_to_erase) {
            Ok(State::FinishedErasingSlot(FinishedErasingSlot {
                buffer: self.buffer,
                chck: self.chck,
                slot: self.slot,
            }))
        } else {
            Ok(State::ErasingSlot(self))
        }
    }
}

pub(super) struct FinishedErasingSlot {
    buffer: BorrowedUpdateBuffer,
    chck: AuxFlashChecksum,
    slot: u32,
}

impl FinishedErasingSlot {
    pub(super) fn into_accepting_data(self) -> AcceptingData {
        AcceptingData {
            buffer: self.buffer,
            chck: self.chck,
            slot: self.slot,
            next_write_offset: 0,
        }
    }
}

pub(super) struct AcceptingData {
    buffer: BorrowedUpdateBuffer,
    chck: AuxFlashChecksum,
    slot: u32,
    next_write_offset: u32,
}

impl AcceptingData {
    pub(super) fn ingest_chunk(
        mut self,
        task: &AuxFlash,
        offset: u32,
        mut data: &[u8],
        aux_flash_size: u32,
    ) -> (IngestDataResult, Result<(), SpError>) {
        // Check that this chunk starts where our data ends.
        let expected_offset = self.next_write_offset + self.buffer.len() as u32;
        if offset != expected_offset
            || offset + data.len() as u32 > aux_flash_size
        {
            return (
                IngestDataResult::NewState(State::AcceptingData(self)),
                Err(SpError::InvalidUpdateChunk),
            );
        }

        while !data.is_empty() {
            data = self.buffer.extend_from_slice(data);

            // Flush this block if it's full or it's the last one.
            if self.buffer.len() == self.buffer.capacity()
                || self.next_write_offset + self.buffer.len() as u32
                    == aux_flash_size
            {
                if let Err(err) = task.write_slot_with_offset(
                    self.slot,
                    self.next_write_offset,
                    &self.buffer,
                ) {
                    return (
                        IngestDataResult::NewState(State::Failed(err)),
                        Err(SpError::UpdateFailed(err as u32)),
                    );
                }

                self.next_write_offset += self.buffer.len() as u32;
                self.buffer.clear();
            }
        }

        if self.next_write_offset == aux_flash_size {
            // Write complete; ensure the chck checks.
            let slot_chck = match task.read_slot_chck(self.slot) {
                Ok(slot_chck) => slot_chck,
                Err(err) => {
                    return (
                        IngestDataResult::NewState(State::Failed(err)),
                        Err(SpError::UpdateFailed(err as u32)),
                    );
                }
            };

            if slot_chck != self.chck {
                let err = AuxFlashError::ChckMismatch;
                return (
                    IngestDataResult::NewState(State::Failed(err)),
                    Err(SpError::UpdateFailed(err as u32)),
                );
            }

            (IngestDataResult::Done(self.buffer), Ok(()))
        } else {
            (
                IngestDataResult::NewState(State::AcceptingData(self)),
                Ok(()),
            )
        }
    }
}
