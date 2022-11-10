// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Stub implementation that has the same types and methods as the real
/// `auxflash` module (our sibling). All method implementations panic: none of
/// them should ever be called.
use crate::mgs_handler::BorrowedUpdateBuffer;
use gateway_messages::{SpError, UpdateId, UpdateStatus};
use userlib::TaskId;

// Provide a fake `AuxFlash` idol client for our parent to import.
pub(super) struct AuxFlash;

impl From<TaskId> for AuxFlash {
    fn from(_: TaskId) -> Self {
        Self
    }
}

// Provide a fake aux flash task slot for our parent to import.
pub(super) struct FakeAuxFlashTaskSlot;

impl FakeAuxFlashTaskSlot {
    pub(super) fn get_task_id(&self) -> TaskId {
        TaskId::UNBOUND
    }
}

#[allow(dead_code)] // we never construct any of these variants
pub(super) enum State {
    ScanningForChck(ScanningForChck),
    ErasingSlot(ErasingSlot),
    FinishedErasingSlot(FinishedErasingSlot),
    AcceptingData(AcceptingData),
    Failed(AuxFlashError),
}

#[allow(dead_code)] // we never construct any of these variants
pub(super) enum ChckScanResult {
    FoundMatch(BorrowedUpdateBuffer),
    NewState(State),
}

#[allow(dead_code)] // we never construct any of these variants
pub(super) enum IngestDataResult {
    Done(BorrowedUpdateBuffer),
    NewState(State),
}

#[allow(dead_code)] // we never construct any of these variants
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub(super) enum AuxFlashError {
    AuxFlashNotAvailable = 0xdead_dead,
}

impl State {
    pub(super) fn new(
        _task: &AuxFlash,
        _buffer: BorrowedUpdateBuffer,
        _chck: [u8; 32],
    ) -> Self {
        panic!()
    }

    pub(super) fn is_preparing(&self) -> bool {
        panic!()
    }

    pub(super) fn status(
        &self,
        _id: UpdateId,
        _total_size: u32,
    ) -> UpdateStatus {
        panic!()
    }
}

pub(super) struct ScanningForChck;

impl ScanningForChck {
    pub(super) fn continue_scanning(self, _task: &AuxFlash) -> ChckScanResult {
        panic!()
    }
}

pub(super) struct ErasingSlot;

impl ErasingSlot {
    pub(super) fn continue_erasing(
        self,
        _task: &AuxFlash,
    ) -> Result<State, AuxFlashError> {
        panic!()
    }
}

pub(super) struct FinishedErasingSlot;

impl FinishedErasingSlot {
    pub(super) fn into_accepting_data(self) -> AcceptingData {
        panic!()
    }
}

pub(super) struct AcceptingData;

impl AcceptingData {
    pub(super) fn ingest_chunk(
        self,
        _task: &AuxFlash,
        _offset: u32,
        _data: &[u8],
        _aux_flash_size: u32,
    ) -> (IngestDataResult, Result<(), SpError>) {
        panic!()
    }
}
