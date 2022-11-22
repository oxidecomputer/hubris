// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{common::CurrentUpdate, ComponentUpdater};
use crate::mgs_handler::{BorrowedUpdateBuffer, UpdateBuffer};
use core::ops::Range;
use drv_sprot_api::SpRot;
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
    Complete,
    Aborted,
}

impl ComponentUpdater for RotUpdate {
    //    const BLOCK_SIZE: usize = lpc55_romapi::FLASH_PAGE_SIZE;
    const BLOCK_SIZE: usize = 512;

    fn prepare(
        &mut self,
        buffer: &'static UpdateBuffer,
        update: ComponentUpdatePrepare,
    ) -> Result<(), SpError> {
        unimplemented!();
    }

    fn is_preparing(&self) -> bool {
        unimplemented!()
    }

    fn step_preparation(&mut self) {
        unimplemented!();
    }

    fn status(&self) -> UpdateStatus {
        UpdateStatus::None
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
