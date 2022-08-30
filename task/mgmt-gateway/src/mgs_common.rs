// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::convert::Infallible;

use crate::{Log, MgsMessage, __RINGBUF};
use drv_update_api::stm32h7::BLOCK_SIZE_BYTES;
use drv_update_api::{Update, UpdateTarget};
use gateway_messages::{
    DiscoverResponse, ResponseError, SpPort, SpState, UpdateChunk, UpdateStart,
};
use mutable_statics::mutable_statics;
use ringbuf::ringbuf_entry;
use tinyvec::ArrayVec;

// TODO How are we versioning SP images? This is a placeholder.
const VERSION: u32 = 1;

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    update_task: Update,
    // TODO: Make this non-`Option` and use new update abort APIs.
    update_buf: Option<&'static mut UpdateBuffer>,
    reset_requested: bool,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            update_task: Update::from(crate::UPDATE_SERVER.get_task_id()),
            update_buf: None,
            reset_requested: false,
        }
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn sp_state(&mut self) -> Result<SpState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

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
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateStart {
            length: update.total_size
        }));

        if let Some(progress) = self.update_buf.as_ref() {
            return Err(ResponseError::UpdateInProgress {
                bytes_received: progress.bytes_written as u32,
            });
        }

        self.update_task
            .prep_image_update(UpdateTarget::Alternate)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        // We can only call `claim_update_buffer_static` once; we bail out above
        // if `self.update_buf` is already `Some(_)`, and after we claim it
        // here, we store that into `self.update_buf` (and never clear it).
        let update_buffer = claim_update_buffer_static();
        update_buffer.total_length = update.total_size as usize;
        self.update_buf = Some(update_buffer);

        Ok(())
    }

    pub(crate) fn update_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateChunk {
            offset: chunk.offset,
        }));

        let update_buf = self
            .update_buf
            .as_mut()
            .ok_or(ResponseError::InvalidUpdateChunk)?;

        update_buf.ingest_chunk(&self.update_task, chunk.offset, data)?;

        Ok(())
    }

    pub(crate) fn reset_prepare(&mut self) -> Result<(), ResponseError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SysResetPrepare));
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

#[derive(Default)]
struct UpdateBuffer {
    total_length: usize,
    bytes_written: usize,
    current_block: ArrayVec<[u8; BLOCK_SIZE_BYTES]>,
}

impl UpdateBuffer {
    fn ingest_chunk(
        &mut self,
        update_task: &Update,
        offset: u32,
        mut data: &[u8],
    ) -> Result<(), ResponseError> {
        // Reject chunks that don't match our current progress.
        if offset as usize != self.bytes_written {
            return Err(ResponseError::UpdateInProgress {
                bytes_received: self.bytes_written as u32,
            });
        }

        // Reject chunks that would go past the total size we're expecting.
        if self.bytes_written + data.len() > self.total_length {
            return Err(ResponseError::InvalidUpdateChunk);
        }

        while !data.is_empty() {
            let cap = self.current_block.capacity() - self.current_block.len();
            assert!(cap > 0);
            let to_copy = usize::min(cap, data.len());

            let current_block_index = self.bytes_written / BLOCK_SIZE_BYTES;
            self.current_block.extend_from_slice(&data[..to_copy]);
            data = &data[to_copy..];
            self.bytes_written += to_copy;

            // If the block is full or this is the final block, send it to the
            // update task.
            if self.current_block.len() == self.current_block.capacity()
                || self.bytes_written == self.total_length
            {
                let result = update_task
                    .write_one_block(current_block_index, &self.current_block)
                    .map_err(|err| ResponseError::UpdateFailed(err as u32));

                // Unconditionally clear our block buffer after attempting to
                // write the block.
                let n = self.current_block.len();
                self.current_block.clear();

                // If writing this block failed, roll back our `bytes_written`
                // counter to the beginning of the block we just tried to write.
                if let Err(err) = result {
                    self.bytes_written -= n;
                    return Err(err);
                }
            }
        }

        // Finalizing the update is implicit (we finalize if we just wrote the
        // last block). Should we make it explict somehow? Maybe that comes with
        // adding auth / code signing?
        if self.bytes_written == self.total_length {
            update_task
                .finish_image_update()
                .map_err(|err| ResponseError::UpdateFailed(err as u32))?;
            ringbuf_entry!(Log::UpdateComplete);
        } else {
            ringbuf_entry!(Log::UpdatePartial {
                bytes_written: self.bytes_written
            });
        }

        Ok(())
    }
}

/// Grabs reference to a static `UpdateBuffer`. Can only be called once!
fn claim_update_buffer_static() -> &'static mut UpdateBuffer {
    // TODO: `mutable_statics!` is currently limited in what inputs it accepts,
    // and in particular only accepts static mut arrays. We only want a single
    // `UpdateBuffer`, so we create an array of length 1 and grab its only
    // element.
    let update_buffer_array = mutable_statics! {
        static mut BUF: [UpdateBuffer; 1] = [Default::default(); _];
    };
    &mut update_buffer_array[0]
}
