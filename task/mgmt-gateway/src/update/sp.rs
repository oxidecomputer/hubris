// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use cfg_if::cfg_if;
use core::ops::{Deref, DerefMut};
use drv_update_api::stm32h7::BLOCK_SIZE_BYTES;
use drv_update_api::{Update, UpdateError, UpdateTarget};
use gateway_messages::{
    ResponseError, SpComponent, SpUpdatePrepare, UpdateId,
    UpdateInProgressStatus, UpdateStatus,
};

#[cfg_attr(not(feature = "auxflash"), path = "sp/stub_auxflash.rs")]
mod auxflash;

use self::auxflash::ChckScanResult;
use self::auxflash::IngestDataResult as AuxFlashIngestDataResult;
use self::auxflash::State as AuxFlashState;

cfg_if! {
    if #[cfg(feature = "auxflash")] {
        use drv_auxflash_api::AuxFlash;

        userlib::task_slot!(AUX_FLASH_SERVER, auxflash);
    } else {
        // If the auxflash feature isn't enabled, we can stub out a fake
        // `AuxFlash` server that we never actually do anything with (other than
        // construct it).
        use userlib::TaskId;

        struct AuxFlash;
        impl From<TaskId> for AuxFlash {
            fn from(_: TaskId) -> Self {
                Self
            }
        }

        struct FakeAuxFlashServer;
        impl FakeAuxFlashServer {
            fn get_task_id(&self) -> TaskId { TaskId::UNBOUND }
        }
        const AUX_FLASH_SERVER: FakeAuxFlashServer = FakeAuxFlashServer;
    }
}

userlib::task_slot!(UPDATE_SERVER, update_server);

static_assertions::const_assert!(
    BLOCK_SIZE_BYTES <= UpdateBuffer::MAX_CAPACITY
);

pub(crate) struct SpUpdate {
    sp_task: Update,
    auxflash_task: AuxFlash,
    current: Option<CurrentUpdate>,
}

impl SpUpdate {
    #[cfg(feature = "hash")]
    pub(crate) const BLOCK_SIZE: usize =
        crate::usize_max(BLOCK_SIZE_BYTES, drv_auxflash_api::PAGE_SIZE_BYTES);
    #[cfg(not(feature = "hash"))]
    pub(crate) const BLOCK_SIZE: usize = BLOCK_SIZE_BYTES;

    pub(crate) fn new() -> Self {
        Self {
            sp_task: Update::from(UPDATE_SERVER.get_task_id()),
            auxflash_task: AuxFlash::from(AUX_FLASH_SERVER.get_task_id()),
            current: None,
        }
    }

    pub(crate) fn prepare(
        &mut self,
        buffer: &'static UpdateBuffer,
        update: SpUpdatePrepare,
    ) -> Result<(), ResponseError> {
        // Do we have an update already in progress?
        match self.current.as_ref().map(|c| c.state()) {
            Some(State::AuxFlash(_))
            | Some(State::FoundMatchingAuxFlashChck { .. })
            | Some(State::AcceptingData { .. }) => {
                return Err(ResponseError::UpdateInProgress(self.status()));
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
            .borrow(SpComponent::SP_ITSELF, BLOCK_SIZE_BYTES)
            .map_err(|component| {
                ResponseError::OtherComponentUpdateInProgress(component)
            })?;

        // Can we handle an auxflash update?
        if update.aux_flash_size > 0 && cfg!(not(feature = "auxflash")) {
            return Err(ResponseError::RequestUnsupportedForSp);
        }

        // Attempt to prepare for an update (erases our flash).
        self.sp_task
            .prep_image_update(UpdateTarget::Alternate)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        let state = if update.aux_flash_size > 0 {
            State::AuxFlash(AuxFlashState::new(
                &self.auxflash_task,
                buffer,
                update.aux_flash_chck,
            ))
        } else {
            State::AcceptingData(AcceptingData {
                buffer,
                next_write_offset: 0,
            })
        };

        self.current = Some(CurrentUpdate::new(
            update.id,
            update.aux_flash_size,
            update.sp_image_size,
            state,
        ));

        Ok(())
    }

    pub(crate) fn is_preparing(&self) -> bool {
        match self.current.as_ref().map(|c| c.state()) {
            Some(State::AuxFlash(s)) => s.is_preparing(),
            _ => false,
        }
    }

    pub(crate) fn step_preparation(&mut self) {
        // Do we have an update?
        let current = match self.current.as_mut() {
            Some(current) => current,
            None => return,
        };

        current.update_state(|state| match state {
            // auxflash states that have prep work to do.
            State::AuxFlash(AuxFlashState::ScanningForChck(scan)) => {
                match scan.continue_scanning(&self.auxflash_task) {
                    ChckScanResult::FoundMatch(mut buffer) => {
                        // Take ownership of `buffer` back, and resize it for
                        // our blocks.
                        buffer
                            .reborrow(SpComponent::SP_ITSELF, BLOCK_SIZE_BYTES);
                        State::FoundMatchingAuxFlashChck { buffer }
                    }
                    ChckScanResult::NewState(s) => State::AuxFlash(s),
                }
            }
            State::AuxFlash(AuxFlashState::ErasingSlot(erase)) => {
                match erase.continue_erasing(&self.auxflash_task) {
                    Ok(s) => State::AuxFlash(s),
                    // TODO we're losing the specific auxflash error :(
                    Err(_) => State::Failed(UpdateError::FlashError),
                }
            }

            // states with no prep work
            State::AuxFlash(AuxFlashState::FinishedErasingSlot(_))
            | State::AuxFlash(AuxFlashState::AcceptingData(_))
            | State::AuxFlash(AuxFlashState::Failed(_))
            | State::FoundMatchingAuxFlashChck { .. }
            | State::AcceptingData(_)
            | State::Complete
            | State::Aborted
            | State::Failed(_) => state,
        });
    }

    pub(crate) fn status(&self) -> UpdateStatus {
        let current = match self.current.as_ref() {
            Some(current) => current,
            None => return UpdateStatus::None,
        };

        match current.state() {
            State::AuxFlash(s) => s.status(current.id(), current.total_size()),
            State::FoundMatchingAuxFlashChck { .. } => {
                UpdateStatus::SpUpdateAuxFlashChckScan {
                    id: current.id(),
                    found_match: true,
                    total_size: current.total_size(),
                }
            }
            State::AcceptingData(accepting) => {
                UpdateStatus::InProgress(UpdateInProgressStatus {
                    id: current.id(),
                    bytes_received: current.aux_flash_size
                        + accepting.next_write_offset
                        + accepting.buffer.len() as u32,
                    total_size: current.total_size(),
                })
            }
            State::Complete => UpdateStatus::Complete(current.id()),
            State::Aborted => UpdateStatus::Aborted(current.id()),
            State::Failed(err) => UpdateStatus::Failed {
                id: current.id(),
                code: *err as u32,
            },
        }
    }

    pub(crate) fn ingest_chunk(
        &mut self,
        component: &SpComponent,
        id: &UpdateId,
        offset: u32,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        // Ensure we are expecting data.
        let current = self
            .current
            .as_mut()
            .ok_or(ResponseError::UpdateNotPrepared)?;

        // Reject chunks that don't match our current update ID.
        if *id != current.id() {
            return Err(ResponseError::InvalidUpdateId {
                sp_update_id: current.id(),
            });
        }

        // Copy fields of `current` so we can borrow it mutably.
        let aux_flash_size = current.aux_flash_size;
        let sp_image_size = current.sp_image_size;

        // Handle aux flash states.
        if let Some(result) = current.update_state_with_result(|state| {
            let auxflash_state = match state {
                State::AuxFlash(s) => s,
                // All other states are handled below.
                _ => return (state, None),
            };

            // We are in an aux flash state - is this chunk aux flash data?
            if *component != SpComponent::SP_AUX_FLASH {
                return (
                    State::AuxFlash(auxflash_state),
                    Some(Err(ResponseError::InvalidUpdateChunk)),
                );
            }

            // Are we in a state where we can accept data?
            let accepting = match auxflash_state {
                AuxFlashState::AcceptingData(a) => a,
                AuxFlashState::FinishedErasingSlot(s) => {
                    s.into_accepting_data()
                }
                AuxFlashState::ScanningForChck(_)
                | AuxFlashState::ErasingSlot(_) => {
                    return (
                        State::AuxFlash(auxflash_state),
                        Some(Err(ResponseError::UpdateNotPrepared)),
                    );
                }
                AuxFlashState::Failed(err) => {
                    return (
                        State::AuxFlash(auxflash_state),
                        Some(Err(ResponseError::UpdateFailed(err as u32))),
                    );
                }
            };

            // Perform the actual data ingest, and map back to a new state.
            let (ingest_result, result) = accepting.ingest_chunk(
                &self.auxflash_task,
                offset,
                data,
                aux_flash_size,
            );
            let new_state = match ingest_result {
                AuxFlashIngestDataResult::NewState(s) => State::AuxFlash(s),
                AuxFlashIngestDataResult::Done(mut buffer) => {
                    // Take ownership of `buffer` back, and resize it for
                    // our blocks.
                    buffer.reborrow(SpComponent::SP_ITSELF, BLOCK_SIZE_BYTES);
                    State::AcceptingData(AcceptingData {
                        buffer,
                        next_write_offset: 0,
                    })
                }
            };
            (new_state, Some(result))
        }) {
            // If our closure above returned `Some(result)`, it handled this
            // chunk and we're done. If it returned `None`, we're not in an
            // auxflash state and we need to handle this chunk as part of the SP
            // update (below).
            return result;
        }

        // We're not in an aux flash state, so check that this chunk is SP data.
        if *component != SpComponent::SP_ITSELF {
            return Err(ResponseError::InvalidUpdateChunk);
        }

        // Handle SP image states.
        current.update_state_with_result(|state| {
            let accepting = match state {
                State::AuxFlash(_) => unreachable!(), // handled above
                State::FoundMatchingAuxFlashChck { buffer } => AcceptingData {
                    buffer,
                    next_write_offset: 0,
                },
                State::AcceptingData(a) => a,
                State::Complete | State::Aborted => {
                    return (state, Err(ResponseError::UpdateNotPrepared))
                }
                State::Failed(err) => {
                    return (
                        state,
                        Err(ResponseError::UpdateFailed(err as u32)),
                    )
                }
            };

            accepting.ingest_chunk(&self.sp_task, sp_image_size, offset, data)
        })
    }

    pub(crate) fn abort(&mut self, id: &UpdateId) -> Result<(), ResponseError> {
        // Do we have an update in progress? If not, nothing to do.
        let current = match self.current.as_mut() {
            Some(current) => current,
            None => return Err(ResponseError::UpdateNotPrepared),
        };

        if *id != current.id() {
            return Err(ResponseError::UpdateInProgress(self.status()));
        }

        match current.state() {
            // Active states - do any work necessary to abort, then set our
            // state to `Aborted`.
            State::AuxFlash(_)
            | State::FoundMatchingAuxFlashChck { .. }
            | State::AcceptingData { .. }
            | State::Failed(_) => {
                match self.sp_task.abort_update() {
                    // Aborting an update that hasn't started yet is fine;
                    // either way our caller is clear to start a new update.
                    Ok(()) | Err(UpdateError::UpdateNotStarted) => {
                        *current.state_mut() = State::Aborted;
                        Ok(())
                    }
                    Err(other) => {
                        Err(ResponseError::UpdateFailed(other as u32))
                    }
                }
            }

            // Update already aborted - aborting it again is a no-op.
            State::Aborted => Ok(()),

            // Update has already completed - too late to abort.
            State::Complete => {
                Err(ResponseError::UpdateInProgress(self.status()))
            }
        }
    }
}

struct CurrentUpdate {
    aux_flash_size: u32,
    sp_image_size: u32,
    common: super::common::CurrentUpdate<State>,
}

impl CurrentUpdate {
    fn new(
        id: UpdateId,
        aux_flash_size: u32,
        sp_image_size: u32,
        state: State,
    ) -> Self {
        Self {
            aux_flash_size,
            sp_image_size,
            common: super::common::CurrentUpdate::new(
                id,
                aux_flash_size + sp_image_size,
                state,
            ),
        }
    }
}

impl Deref for CurrentUpdate {
    type Target = super::common::CurrentUpdate<State>;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl DerefMut for CurrentUpdate {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

enum State {
    AuxFlash(AuxFlashState),
    FoundMatchingAuxFlashChck { buffer: BorrowedUpdateBuffer },
    AcceptingData(AcceptingData),
    Complete,
    Aborted,
    Failed(UpdateError),
}

struct AcceptingData {
    buffer: BorrowedUpdateBuffer,
    next_write_offset: u32,
}

impl AcceptingData {
    fn ingest_chunk(
        mut self,
        sp_task: &Update,
        sp_image_size: u32,
        offset: u32,
        mut data: &[u8],
    ) -> (State, Result<(), ResponseError>) {
        // Check that this chunk starts where our data ends.
        let expected_offset = self.next_write_offset + self.buffer.len() as u32;
        if offset != expected_offset
            || offset + data.len() as u32 > sp_image_size
        {
            return (
                State::AcceptingData(self),
                Err(ResponseError::InvalidUpdateChunk),
            );
        }

        while !data.is_empty() {
            data = self.buffer.extend_from_slice(data);

            // Flush this block if it's full or it's the last one.
            if self.buffer.len() == self.buffer.capacity()
                || self.next_write_offset + self.buffer.len() as u32
                    == sp_image_size
            {
                let block = self.next_write_offset as usize / BLOCK_SIZE_BYTES;
                if let Err(err) = sp_task.write_one_block(block, &self.buffer) {
                    return (
                        State::Failed(err),
                        Err(ResponseError::UpdateFailed(err as u32)),
                    );
                }

                self.next_write_offset += self.buffer.len() as u32;
                self.buffer.clear();
            }
        }

        // Did we write the last block?
        if self.next_write_offset == sp_image_size {
            match sp_task.finish_image_update() {
                Ok(()) => (State::Complete, Ok(())),
                Err(err) => (
                    State::Failed(err),
                    Err(ResponseError::UpdateFailed(err as u32)),
                ),
            }
        } else {
            (State::AcceptingData(self), Ok(()))
        }
    }
}
