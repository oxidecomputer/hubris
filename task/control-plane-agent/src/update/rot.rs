// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{common::CurrentUpdate, ComponentUpdater};
use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use core::ops::Range;
use drv_sprot_api::MsgError as SprotError;
use drv_sprot_api::SpRot;
use drv_update_api::lpc55::BLOCK_SIZE_BYTES;
use drv_update_api::{ImageVersion, UpdateError, UpdateTarget};

use gateway_messages::{
    ComponentUpdatePrepare, SpComponent, SpError, UpdateId,
    UpdateInProgressStatus, UpdatePreparationProgress, UpdatePreparationStatus,
    UpdateStatus,
};

userlib::task_slot!(SPROT, sprot);

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
        let buffer =
            buffer.borrow(SpComponent::ROT, Self::BLOCK_SIZE).map_err(
                |component| SpError::OtherComponentUpdateInProgress(component),
            )?;

        // Which target are we updating?
        let target = match update.slot {
            0 => UpdateTarget::ImageA,
            1 => UpdateTarget::ImageB,
            _ => return Err(SpError::InvalidSlotForComponent),
        };

        self.task
            .prep_image_update(target)
            .map_err(|err| SpError::UpdateFailed(err as u32))?;

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
        unimplemented!();
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
            State::Failed(err) => UpdateStatus::Failed {
                id: current.id(),
                code: *err as u32,
            },
        }
    }

    fn ingest_chunk(
        &mut self,
        id: &UpdateId,
        offset: u32,
        data: &[u8],
    ) -> Result<(), SpError> {
        unimplemented!()
    }

    fn abort(&mut self, id: &UpdateId) -> Result<(), SpError> {
        unimplemented!()
    }
}
