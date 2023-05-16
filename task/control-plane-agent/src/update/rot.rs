// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{common::CurrentUpdate, ComponentUpdater};
use crate::mgs_common::SPROT;
use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use drv_lpc55_update_api::{UpdateTarget, BLOCK_SIZE_BYTES};
use drv_sprot_api::{SpRot, SprotError};
use ringbuf::{ringbuf, ringbuf_entry};

use gateway_messages::{
    ComponentUpdatePrepare, SpComponent, SpError, UpdateId,
    UpdateInProgressStatus, UpdateStatus,
};

ringbuf!(Trace, 64, Trace::None);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Trace {
    None,
    IngestChunkInput { offset: u32, len: usize },
    IngestChunkState { offset: u32, len: usize },
    WriteOneBlock(u32, usize, usize),
}

pub(crate) struct RotUpdate {
    task: SpRot,
    current: Option<CurrentUpdate<State>>,
}

impl RotUpdate {
    pub(crate) fn new() -> Self {
        RotUpdate {
            task: SpRot::from(SPROT.get_task_id()),
            current: None,
        }
    }
}

enum State {
    AcceptingData {
        buffer: BorrowedUpdateBuffer,
        next_write_offset: u32,
    },
    Complete,
    Aborted,
    Failed(SprotError),
}

impl ComponentUpdater for RotUpdate {
    const BLOCK_SIZE: usize = BLOCK_SIZE_BYTES;

    fn prepare(
        &mut self,
        buffer: &'static UpdateBuffer,
        update: ComponentUpdatePrepare,
    ) -> Result<(), SpError> {
        match self.current.as_ref().map(CurrentUpdate::state) {
            Some(State::AcceptingData { .. }) => {
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
            .borrow(update.component, Self::BLOCK_SIZE)
            .map_err(SpError::OtherComponentUpdateInProgress)?;

        // Which target are we updating?
        let target = match (update.component, update.slot) {
            (SpComponent::ROT, 0) => UpdateTarget::ImageA,
            (SpComponent::ROT, 1) => UpdateTarget::ImageB,
            (SpComponent::STAGE0, 0) => UpdateTarget::Bootloader,
            _ => return Err(SpError::InvalidSlotForComponent),
        };

        self.task.prep_image_update(target)?;

        self.current = Some(CurrentUpdate::new(
            update.id,
            update.total_size,
            State::AcceptingData {
                buffer,
                next_write_offset: 0,
            },
        ));

        Ok(())
    }

    fn is_preparing(&self) -> bool {
        false
    }

    fn step_preparation(&mut self) {
        // There's no stepping for an RoT update
        // Unreachable because `is_preparing` always returns `false`.
        unreachable!();
    }

    fn status(&self) -> UpdateStatus {
        let current = match self.current.as_ref() {
            Some(current) => current,
            None => return UpdateStatus::None,
        };

        match current.state() {
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
            // We can't actually reveal the real error here because
            // an error from sprot doesn't map directly to a u32 anymore.
            // We also can't put the real error inside `UpdateStatus::Failed`
            // because `UpdateStatus` is already included in `SpError`, and therefore
            // the definition for `UpdateStatus` would be recursive. We can't "box" our way
            // out of this because we are in a `no_std` context. For now, until we
            // come up with a better idea, we just put `9999` here to indicate
            // "RoT update error".
            //
            // This isn't really that big a deal, since the error will be
            // apparent elsewhere in most cases such as the return value of the
            // failing operation itself and in logs.
            State::Failed(_) => UpdateStatus::Failed {
                id: current.id(),
                code: 9999,
            },
        }
    }

    fn ingest_chunk(
        &mut self,
        id: &UpdateId,
        offset: u32,
        mut data: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry!(Trace::IngestChunkInput {
            offset,
            len: data.len()
        });

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
            State::Complete | State::Aborted => {
                return Err(SpError::UpdateNotPrepared)
            }
            State::Failed(err) => return Err((*err).into()),
        };

        ringbuf_entry!(Trace::IngestChunkState {
            offset: *next_write_offset,
            len: buffer.len()
        });

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
                let block_num = *next_write_offset / Self::BLOCK_SIZE as u32;
                ringbuf_entry!(Trace::WriteOneBlock(
                    block_num,
                    buffer.len(),
                    buffer.capacity()
                ));
                if let Err(err) = self.task.write_one_block(block_num, buffer) {
                    *current.state_mut() = State::Failed(err);

                    return Err(err.into());
                }

                *next_write_offset += buffer.len() as u32;
                buffer.clear();
            }
        }

        // Finish the update if we just wrote the last block.
        if *next_write_offset == total_size {
            if let Err(err) = self.task.finish_image_update() {
                *current.state_mut() = State::Failed(err);
                return Err(err.into());
            }
            *current.state_mut() = State::Complete;
        }

        Ok(())
    }

    fn abort(&mut self, id: &UpdateId) -> Result<(), SpError> {
        // Ensure we are expecting data.
        let current =
            self.current.as_mut().ok_or(SpError::UpdateNotPrepared)?;

        // Only proceed if the requested ID matches ours.
        if *id != current.id() {
            return Err(SpError::UpdateInProgress(self.status()));
        }

        match current.state() {
            State::AcceptingData { .. } | State::Failed(_) => {
                match self.task.abort_update() {
                    Ok(()) => {
                        *current.state_mut() = State::Aborted;
                        Ok(())
                    }
                    Err(err) => Err(err.into()),
                }
            }
            // Update already aborted - aborting it again is a no-op.
            State::Aborted => Ok(()),
            // Update has already completed - too late to abort.
            State::Complete => Err(SpError::UpdateInProgress(self.status())),
        }
    }
}
