// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{common::CurrentUpdate, ComponentUpdater};
use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use core::ops::Range;
use drv_hf_api::{
    HfDevSelect, HfError, HfProtectMode, HostFlash, PAGE_SIZE_BYTES,
    SECTOR_SIZE_BYTES,
};
use gateway_messages::{
    ComponentUpdatePrepare, SpComponent, SpError, UpdateId,
    UpdateInProgressStatus, UpdatePreparationProgress, UpdatePreparationStatus,
    UpdateStatus,
};

userlib::task_slot!(HOST_FLASH, hf);

pub(crate) struct HostFlashUpdate {
    task: HostFlash,
    current: Option<CurrentUpdate<State>>,
}

impl HostFlashUpdate {
    pub(crate) fn new() -> Self {
        Self {
            task: HostFlash::from(HOST_FLASH.get_task_id()),
            current: None,
        }
    }

    pub(crate) fn active_slot(&self) -> Result<u16, SpError> {
        match self
            .task
            .get_dev()
            .map_err(|err| SpError::ComponentOperationFailed(err as u32))?
        {
            HfDevSelect::Flash0 => Ok(0),
            HfDevSelect::Flash1 => Ok(1),
        }
    }

    pub(crate) fn set_active_slot(
        &self,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        let slot = match slot {
            0 => HfDevSelect::Flash0,
            1 => HfDevSelect::Flash1,
            _ => return Err(SpError::InvalidSlotForComponent),
        };

        // Attempt to swap to the chosen slot, returning a "slot busy" error if
        // we don't have control over the host flash.
        match self.task.set_dev(slot) {
            Ok(()) => Ok(()),

            // Otherwise, things went wrong; translate if possible:
            Err(HfError::NotMuxedToSP) => Err(SpError::UpdateSlotBusy),
            Err(err) => Err(SpError::UpdateFailed(err as u32)),
        }?;
        if persist {
            match self.task.write_persistent_data(slot) {
                Ok(()) => Ok(()),
                Err(HfError::NotMuxedToSP) => Err(SpError::UpdateSlotBusy),
                Err(err) => Err(SpError::UpdateFailed(err as u32)),
            }
        } else {
            Ok(())
        }
    }
}

// Ensure our `UpdateBuffer` type is sized large enough for us.
static_assertions::const_assert!(
    HostFlashUpdate::BLOCK_SIZE <= UpdateBuffer::MAX_CAPACITY
);

impl ComponentUpdater for HostFlashUpdate {
    const BLOCK_SIZE: usize = PAGE_SIZE_BYTES;

    type UpdatePrepare = ComponentUpdatePrepare;
    type SubComponent = ();

    fn prepare(
        &mut self,
        buffer: &'static UpdateBuffer,
        update: ComponentUpdatePrepare,
    ) -> Result<(), SpError> {
        // Do we have an update already in progress?
        match self.current.as_ref().map(CurrentUpdate::state) {
            Some(State::ErasingSectors { .. })
            | Some(State::AcceptingData { .. }) => {
                return Err(SpError::UpdateInProgress(self.status()));
            }
            Some(State::Complete)
            | Some(State::Aborted)
            | Some(State::Failed(_))
            | None => {
                // All of these states are "done", and we're fine to start a new
                // update.
            }
        }

        // Can we lock the update buffer?
        let buffer = buffer
            .borrow(SpComponent::HOST_CPU_BOOT_FLASH, Self::BLOCK_SIZE)
            .map_err(|component| {
                SpError::OtherComponentUpdateInProgress(component)
            })?;

        // Update the currently-active slot so we can write to it, but don't
        // persist those changes to non-volatile memory.
        self.set_active_slot(update.slot, false)?;

        // What is the total capacity of the device?
        let capacity = self
            .task
            .capacity()
            .map_err(|err| SpError::UpdateFailed(err as u32))?;

        if update.total_size as usize > capacity {
            return Err(SpError::UpdateIsTooLarge);
        }

        // How many total sectors do we need to erase? For gimlet, we know that
        // capacity is an exact multiple of the sector size, which is probably
        // a safe assumption for future parts as well. We'll fail here if that's
        // untrue, which will require reworking how we erase the target slot.
        if capacity % SECTOR_SIZE_BYTES != 0 {
            // We don't have an error case for "our assumptions are wrong", so
            // we'll fill in an easily-greppable update failure code. In case it
            // shows up in logs in base 10, 0x1de_0001 == 31326209.
            return Err(SpError::UpdateFailed(0x1de_0001));
        }
        let num_sectors = (capacity / SECTOR_SIZE_BYTES) as u32;

        // Note that we preserve sector 0, which is used for Hubris-level
        // persistent data.
        self.current = Some(CurrentUpdate::new(
            update.id,
            update.total_size,
            State::ErasingSectors {
                buffer,
                sectors_to_erase: 1..num_sectors,
            },
        ));

        Ok(())
    }

    fn is_preparing(&self) -> bool {
        match self.current.as_ref().map(CurrentUpdate::state) {
            Some(State::ErasingSectors { .. }) => true,
            Some(State::AcceptingData { .. })
            | Some(State::Complete)
            | Some(State::Failed(_))
            | Some(State::Aborted)
            | None => false,
        }
    }

    fn step_preparation(&mut self) {
        let current = match self.current.as_mut() {
            Some(current) => current,
            None => return,
        };

        current.update_state(|state| {
            let (buffer, mut sectors_to_erase) = match state {
                State::ErasingSectors {
                    buffer,
                    sectors_to_erase,
                } => (buffer, sectors_to_erase),
                State::AcceptingData { .. }
                | State::Complete
                | State::Failed(_)
                | State::Aborted => {
                    // Nothing to prepare in any of these states; put it back.
                    return state;
                }
            };

            let addr = sectors_to_erase.start * SECTOR_SIZE_BYTES as u32;

            // During construction of the State::ErasingSectors object, we
            // should have configured it to start at sector 1; using
            // HfProtectMode::ProtectSector0 guards against mistakes.
            match self.task.sector_erase(addr, HfProtectMode::ProtectSector0) {
                Ok(()) => {
                    sectors_to_erase.start += 1;
                    if sectors_to_erase.start == sectors_to_erase.end {
                        State::AcceptingData {
                            buffer,
                            next_write_offset: 0,
                        }
                    } else {
                        State::ErasingSectors {
                            buffer,
                            sectors_to_erase,
                        }
                    }
                }
                Err(err) => State::Failed(err),
            }
        });
    }

    fn status(&self) -> UpdateStatus {
        let current = match self.current.as_ref() {
            Some(current) => current,
            None => return UpdateStatus::None,
        };

        match current.state() {
            State::ErasingSectors {
                sectors_to_erase, ..
            } => UpdateStatus::Preparing(UpdatePreparationStatus {
                id: current.id(),
                progress: Some(UpdatePreparationProgress {
                    current: sectors_to_erase.start,
                    total: sectors_to_erase.end,
                }),
            }),
            State::AcceptingData {
                buffer,
                next_write_offset,
            } => UpdateStatus::InProgress(UpdateInProgressStatus {
                id: current.id(),
                bytes_received: next_write_offset + buffer.len() as u32,
                total_size: current.total_size(),
            }),
            State::Complete => UpdateStatus::Complete(current.id()),
            State::Aborted => UpdateStatus::Aborted(current.id()),
            State::Failed(err) => UpdateStatus::Failed {
                id: current.id(),
                code: *err as u32,
            },
        }
    }

    fn ingest_chunk(
        &mut self,
        _sub: &(),
        id: &UpdateId,
        offset: u32,
        mut data: &[u8],
    ) -> Result<(), SpError> {
        // Ensure we are expecting data.
        let current =
            self.current.as_mut().ok_or(SpError::UpdateNotPrepared)?;

        let current_id = current.id();
        let total_size = current.total_size();

        let (buffer, next_write_offset) = match current.state_mut() {
            State::AcceptingData {
                buffer,
                next_write_offset,
            } => (buffer, next_write_offset),
            State::ErasingSectors { .. } | State::Complete | State::Aborted => {
                return Err(SpError::UpdateNotPrepared)
            }
            State::Failed(err) => {
                return Err(SpError::UpdateFailed(*err as u32))
            }
        };

        // Reject chunks that don't match our current update ID.
        if *id != current_id {
            return Err(SpError::InvalidUpdateId {
                sp_update_id: current_id,
            });
        }

        // Reject chunks that don't match the offset we expect or that would go
        // past the total size we're expecting.
        let expected_offset = *next_write_offset + buffer.len() as u32;
        if offset != expected_offset
            || expected_offset + data.len() as u32 > total_size
        {
            return Err(SpError::InvalidUpdateChunk);
        }

        while !data.is_empty() {
            data = buffer.extend_from_slice(data);

            // Flush this block if it's full or it's the last one.
            if buffer.len() == buffer.capacity()
                || *next_write_offset + buffer.len() as u32 == total_size
            {
                // Alright, this is a little tricky: we want to preserve sector
                // 0 of the host flash, because it's used by Hubris to store
                // bookkeeping information (e.g. what flash slot to select).
                //
                // However, the host that's sending us data doesn't necessarily
                // know about this limitation.  We skip bytes up until the end
                // of sector 0, after checking that they are all 0xFF.
                let skip_bytes = SECTOR_SIZE_BYTES
                    .saturating_sub(*next_write_offset as usize)
                    .min(buffer.len());

                if buffer[0..skip_bytes].iter().any(|b| *b != 0xFF) {
                    let err = HfError::Sector0IsReserved;
                    *current.state_mut() = State::Failed(err);
                    return Err(SpError::UpdateFailed(err as u32));
                }

                if skip_bytes < buffer.len() {
                    if let Err(err) = self.task.page_program(
                        *next_write_offset + skip_bytes as u32,
                        HfProtectMode::ProtectSector0,
                        &buffer[skip_bytes..],
                    ) {
                        *current.state_mut() = State::Failed(err);
                        return Err(SpError::UpdateFailed(err as u32));
                    }
                }

                *next_write_offset += buffer.len() as u32;
                buffer.clear();
            }
        }

        // Nothing special to do after the last block write?
        // Should we set the device back to what it was if we had to change it
        // to write this update?
        if *next_write_offset == total_size {
            *current.state_mut() = State::Complete;
        }

        Ok(())
    }

    fn abort(&mut self, id: &UpdateId) -> Result<(), SpError> {
        // Do we have an update in progress? If not, nothing to do.
        let current = match self.current.as_mut() {
            Some(current) => current,
            None => return Err(SpError::UpdateNotPrepared),
        };

        // Only proceed if the requested ID matches ours.
        if *id != current.id() {
            return Err(SpError::UpdateInProgress(self.status()));
        }

        match current.state() {
            // Active states - do any work necessary to abort (none for host
            // flash), then set our state to `Aborted`.
            State::ErasingSectors { .. }
            | State::AcceptingData { .. }
            | State::Failed(_) => {
                // TODO should we erase the slot? TODO should we set_dev() back
                // to what it was (if we changed it)?
                *current.state_mut() = State::Aborted;
                Ok(())
            }

            // Update already aborted - aborting it again is a no-op.
            State::Aborted => Ok(()),

            // Update has already completed - too late to abort.
            State::Complete => Err(SpError::UpdateInProgress(self.status())),
        }
    }
}

enum State {
    ErasingSectors {
        buffer: BorrowedUpdateBuffer,
        sectors_to_erase: Range<u32>,
    },
    AcceptingData {
        buffer: BorrowedUpdateBuffer,
        next_write_offset: u32,
    },
    Complete,
    Aborted,
    Failed(HfError),
}
