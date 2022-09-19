// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{update_buffer::UpdateBuffer, Log, MgsMessage};
use core::convert::Infallible;
use drv_update_api::stm32h7::BLOCK_SIZE_BYTES;
use drv_update_api::{Update, UpdateError, UpdateTarget};
use gateway_messages::{
    DiscoverResponse, ResponseError, SpComponent, SpPort, SpState, UpdateChunk,
    UpdateId, UpdatePrepare, UpdateStatus,
};
use ringbuf::ringbuf_entry_root;

// TODO How are we versioning SP images? This is a placeholder.
const VERSION: u32 = 1;

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    update_task: Update,
    update_buf: UpdateBuffer<Update, BLOCK_SIZE_BYTES>,
    reset_requested: bool,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            update_task: Update::from(crate::UPDATE_SERVER.get_task_id()),
            update_buf: UpdateBuffer::new(
                claim_update_buffer_static(),
                // callback to write one block
                |update_task, block_index, data| {
                    update_task
                        .write_one_block(block_index, data)
                        .map_err(|err| ResponseError::UpdateFailed(err as u32))
                },
                // callback to finalize after all blocks written
                |update_task| {
                    update_task
                        .finish_image_update()
                        .map_err(|err| ResponseError::UpdateFailed(err as u32))
                },
            ),
            reset_requested: false,
        }
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn sp_state(&mut self) -> Result<SpState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SpState));

        // TODO Replace with the real serial number once it's available; for now
        // use the stm32 96-bit uid
        let mut serial_number = [0; 16];
        for (to, from) in serial_number.iter_mut().zip(
            drv_stm32xx_uid::read_uid()
                .iter()
                .map(|x| x.to_be_bytes())
                .flatten(),
        ) {
            *to = from;
        }

        Ok(SpState {
            serial_number,
            version: VERSION,
        })
    }

    pub(crate) fn update_prepare(
        &mut self,
        update: UpdatePrepare,
    ) -> Result<(), ResponseError> {
        // We should only be called to update the SP itself.
        if update.component != SpComponent::SP_ITSELF {
            panic!();
        }

        // SP only has one "slot" (the alternate bank).
        if update.slot != 0 {
            return Err(ResponseError::InvalidSlotForComponent);
        }

        self.update_buf.ensure_no_update_in_progress()?;

        self.update_task
            .prep_image_update(UpdateTarget::Alternate)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        self.update_buf.start(update.id, update.total_size);

        Ok(())
    }

    pub(crate) fn update_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        self.update_buf
            .ingest_chunk(&chunk.id, &self.update_task, chunk.offset, data)
            .map(|_progress| ())
    }

    pub(crate) fn status(&self) -> UpdateStatus {
        self.update_buf.status()
    }

    pub(crate) fn update_abort(
        &mut self,
        id: &UpdateId,
    ) -> Result<(), ResponseError> {
        // We will allow the abort if either:
        //
        // 1. We have an in-progress update that matches `id`
        // 2. We do not have an in-progress update
        //
        // We only want to return an error if we have a _different_ in-progress
        // update.
        if let Some(in_progress_id) = self.update_buf.in_progress_update_id() {
            if id != in_progress_id {
                return Err(ResponseError::UpdateInProgress(
                    self.update_buf.status(),
                ));
            }
        }

        match self.update_task.abort_update() {
            // Aborting an update that hasn't started yet is fine; either way
            // our caller is clear to start a new update.
            Ok(()) | Err(UpdateError::UpdateNotStarted) => {
                self.update_buf.abort();
                Ok(())
            }
            Err(other) => Err(ResponseError::UpdateFailed(other as u32)),
        }
    }

    pub(crate) fn reset_prepare(&mut self) -> Result<(), ResponseError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SysResetPrepare));
        self.reset_requested = true;
        Ok(())
    }

    pub(crate) fn reset_trigger(
        &mut self,
    ) -> Result<Infallible, ResponseError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        if !self.reset_requested {
            return Err(ResponseError::SysResetTriggerWithoutPrepare);
        }

        let jefe = task_jefe_api::Jefe::from(crate::JEFE.get_task_id());
        jefe.request_reset();

        // If `request_reset()` returns, something has gone very wrong.
        panic!()
    }
}

fn claim_update_buffer_static(
) -> &'static mut heapless::Vec<u8, BLOCK_SIZE_BYTES> {
    use core::sync::atomic::{AtomicBool, Ordering};

    static mut SP_UPDATE_BUF: heapless::Vec<u8, BLOCK_SIZE_BYTES> =
        heapless::Vec::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `SP_UPDATE_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut SP_UPDATE_BUF }
}
