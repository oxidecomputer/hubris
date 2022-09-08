// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{update_buffer::UpdateBuffer, Log, MgsMessage};
use core::convert::Infallible;
use drv_update_api::stm32h7::BLOCK_SIZE_BYTES;
use drv_update_api::{Update, UpdateTarget};
use gateway_messages::{
    DiscoverResponse, ResponseError, SpPort, SpState, UpdateChunk, UpdateStart,
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

    pub(crate) fn update_start(
        &mut self,
        update: UpdateStart,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateStart {
            length: update.total_size
        }));

        self.update_buf.ensure_no_update_in_progress()?;

        self.update_task
            .prep_image_update(UpdateTarget::Alternate)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        self.update_buf.start(update.total_size as usize);

        Ok(())
    }

    pub(crate) fn update_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
            offset: chunk.offset,
        }));

        self.update_buf
            .ingest_chunk(&self.update_task, chunk.offset, data)
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

/// Grabs reference to a static `UpdateBuffer`. Can only be called once!
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
