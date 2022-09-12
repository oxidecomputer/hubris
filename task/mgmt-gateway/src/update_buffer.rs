// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Log;
use gateway_messages::ResponseError;
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
    total_length: usize,
    bytes_written: usize,
    current_block: &'static mut heapless::Vec<u8, BLOCK_SIZE>,
    update_in_progress: bool,
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
            total_length: 0,
            bytes_written: 0,
            current_block: buf,
            update_in_progress: false,
            write_block_fn,
            finalize_fn,
        }
    }

    pub fn ensure_no_update_in_progress(&self) -> Result<(), ResponseError> {
        if self.update_in_progress {
            Err(ResponseError::UpdateInProgress {
                bytes_received: self.bytes_written as u32,
            })
        } else {
            Ok(())
        }
    }

    /// Panics if an update is in progress; use
    /// [`ensure_no_update_in_progress()`] first.
    pub fn start(&mut self, total_length: usize) {
        if self.update_in_progress {
            panic!();
        }

        self.update_in_progress = true;
        self.total_length = total_length;
        self.bytes_written = 0;
        self.current_block.clear();
    }

    pub fn ingest_chunk(
        &mut self,
        user_data: &T,
        offset: u32,
        mut data: &[u8],
    ) -> Result<(), ResponseError> {
        // Reject chunks if we don't have an update in progress.
        if !self.update_in_progress {
            return Err(ResponseError::InvalidUpdateChunk);
        }

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

            let current_block_index = self.bytes_written / BLOCK_SIZE;
            self.current_block
                .extend_from_slice(&data[..to_copy])
                .unwrap_lite();
            data = &data[to_copy..];
            self.bytes_written += to_copy;

            // If the block is full or this is the final block, send it to the
            // update task.
            if self.current_block.len() == self.current_block.capacity()
                || self.bytes_written == self.total_length
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
            (self.finalize_fn)(user_data)?;
            ringbuf_entry_root!(Log::UpdateComplete);
        } else {
            ringbuf_entry_root!(Log::UpdatePartial {
                bytes_written: self.bytes_written
            });
        }

        Ok(())
    }
}
