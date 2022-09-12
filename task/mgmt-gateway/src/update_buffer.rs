// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Log;
use gateway_messages::{
    ResponseError, UpdateId, UpdateInProgressStatus, UpdateStatus,
};
use ringbuf::ringbuf_entry_root;
use userlib::UnwrapLite;

/// Type alias for a callback function that writes a single block of data.
pub type WriteBlockFn<T> = fn(
    user_data: &T,
    block_index: usize,
    block_data: &[u8],
) -> Result<(), ResponseError>;

/// Type alias for a callback function that finalizes an update after all blocks
/// have been successfully written.
pub type FinalizeFn<T> = fn(user_data: &T) -> Result<(), ResponseError>;

/// `UpdateBuffer` provides common logic for apply updates over the management
/// network, assuming a common pattern of:
///
/// 1. A fixed block size.
/// 2. Update size is known when the update begins, but data arives in
///    arbitrarily-sized chunks.
/// 3. A function to call for each complete block (and the final, potentially
///    short block).
/// 4. A function to call once all blocks have been written.
pub struct UpdateBuffer<T, const BLOCK_SIZE: usize> {
    // We never set `status` to `UpdateStatus::Preparing`, so in all matches
    // below we panic on that arm. If there is a long-running preparation
    // process, our caller will handle that status report.
    status: UpdateStatus,
    current_block: &'static mut heapless::Vec<u8, BLOCK_SIZE>,
    write_block_fn: WriteBlockFn<T>,
    finalize_fn: FinalizeFn<T>,
}

impl<T, const BLOCK_SIZE: usize> UpdateBuffer<T, BLOCK_SIZE> {
    pub fn new(
        buf: &'static mut heapless::Vec<u8, BLOCK_SIZE>,
        write_block_fn: WriteBlockFn<T>,
        finalize_fn: FinalizeFn<T>,
    ) -> Self {
        Self {
            status: UpdateStatus::None,
            current_block: buf,
            write_block_fn,
            finalize_fn,
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status
    }

    pub fn ensure_no_update_in_progress(&self) -> Result<(), ResponseError> {
        match self.status {
            UpdateStatus::Preparing { .. } => panic!(),
            status @ UpdateStatus::InProgress(_) => {
                Err(ResponseError::UpdateInProgress(status))
            }
            UpdateStatus::None
            | UpdateStatus::Complete(_)
            | UpdateStatus::Aborted(_) => Ok(()),
        }
    }

    pub fn in_progress_update_id(&self) -> Option<&UpdateId> {
        match &self.status {
            UpdateStatus::Preparing { .. } => panic!(),
            UpdateStatus::InProgress(status) => Some(&status.id),
            UpdateStatus::None
            | UpdateStatus::Complete(_)
            | UpdateStatus::Aborted(_) => None,
        }
    }

    /// Panics if an update is in progress; use
    /// [`ensure_no_update_in_progress()`] first.
    pub fn start(&mut self, update_id: UpdateId, total_size: u32) {
        if self.ensure_no_update_in_progress().is_err() {
            panic!();
        }

        self.status = UpdateStatus::InProgress(UpdateInProgressStatus {
            id: update_id,
            bytes_received: 0,
            total_size,
        });
        self.current_block.clear();
    }

    pub fn abort(&mut self) {
        match self.status {
            UpdateStatus::Preparing { .. } => panic!(),
            UpdateStatus::InProgress(status) => {
                self.status = UpdateStatus::Aborted(status.id);
            }
            UpdateStatus::None
            | UpdateStatus::Complete(_)
            | UpdateStatus::Aborted(_) => (),
        }
    }

    pub fn ingest_chunk(
        &mut self,
        update_id: &UpdateId,
        user_data: &T,
        offset: u32,
        mut data: &[u8],
    ) -> Result<(), ResponseError> {
        // Check that we have an update currently in progress.
        let status = match &mut self.status {
            UpdateStatus::Preparing { .. } => panic!(),
            UpdateStatus::InProgress(status) => status,
            UpdateStatus::None
            | UpdateStatus::Complete(_)
            | UpdateStatus::Aborted(_) => {
                return Err(ResponseError::UpdateNotPrepared);
            }
        };

        // Reject chunks that don't match our current update ID.
        if &status.id != update_id {
            return Err(ResponseError::InvalidUpdateId {
                sp_update_id: status.id,
            });
        }

        // Reject chunks that don't match our current progress.
        if offset != status.bytes_received {
            return Err(ResponseError::UpdateInProgress(
                UpdateStatus::InProgress(*status),
            ));
        }

        // Reject chunks that would go past the total size we're expecting.
        if status.bytes_received + data.len() as u32 > status.total_size {
            return Err(ResponseError::InvalidUpdateChunk);
        }

        while !data.is_empty() {
            let cap = self.current_block.capacity() - self.current_block.len();
            assert!(cap > 0);
            let to_copy = usize::min(cap, data.len());

            let current_block_index =
                status.bytes_received as usize / BLOCK_SIZE;
            self.current_block
                .extend_from_slice(&data[..to_copy])
                .unwrap_lite();
            data = &data[to_copy..];
            status.bytes_received += to_copy as u32;

            // If the block is full or this is the final block, send it to the
            // update task.
            if self.current_block.len() == self.current_block.capacity()
                || status.bytes_received == status.total_size
            {
                let result = (self.write_block_fn)(
                    user_data,
                    current_block_index,
                    &self.current_block,
                );

                // Unconditionally clear our block buffer after attempting to
                // write the block.
                let n = self.current_block.len();
                self.current_block.clear();

                // If writing this block failed, roll back our `bytes_received`
                // counter to the beginning of the block we just tried to write.
                if let Err(err) = result {
                    status.bytes_received -= n as u32;
                    return Err(err);
                }
            }
        }

        // Finalizing the update is implicit (we finalize if we just wrote the
        // last block). Should we make it explict somehow? Maybe that comes with
        // adding auth / code signing?
        if status.bytes_received == status.total_size {
            (self.finalize_fn)(user_data)?;
            self.status = UpdateStatus::Complete(*update_id);
            ringbuf_entry_root!(Log::UpdateComplete);
            Ok(())
        } else {
            ringbuf_entry_root!(Log::UpdatePartial {
                bytes_written: status.bytes_received
            });
            Ok(())
        }
    }
}
