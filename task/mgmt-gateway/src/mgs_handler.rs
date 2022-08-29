// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::convert::Infallible;

use crate::{Log, MgsMessage, UsartHandler, __RINGBUF};
use drv_update_api::stm32h7::BLOCK_SIZE_BYTES;
use drv_update_api::{Update, UpdateTarget};
use gateway_messages::{
    sp_impl::SocketAddrV6, sp_impl::SpHandler, BulkIgnitionState,
    DiscoverResponse, IgnitionCommand, IgnitionState, ResponseError,
    SpComponent, SpPort, SpState, UpdateChunk, UpdateStart,
};
use mutable_statics::mutable_statics;
use ringbuf::ringbuf_entry;
use tinyvec::ArrayVec;

// TODO How are we versioning SP images? This is a placeholder.
const VERSION: u32 = 1;

pub(crate) struct UsartFlush<'a> {
    pub(crate) usart: &'a mut UsartHandler,
    pub(crate) destination: SocketAddrV6,
    pub(crate) port: SpPort,
}

pub(crate) struct MgsHandler {
    pub(crate) usart: UsartHandler,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
    serial_console_write_offset: u64,
    update_task: Update,
    update_progress: Option<&'static mut UpdateBuffer>,
    reset_requested: bool,
}

impl MgsHandler {
    pub(crate) fn new(usart: UsartHandler) -> Self {
        Self {
            usart,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
            update_task: Update::from(crate::UPDATE_SERVER.get_task_id()),
            update_progress: None,
            reset_requested: false,
        }
    }

    pub(crate) fn needs_usart_flush_to_mgs(&self) -> bool {
        self.usart.should_flush_to_mgs()
    }

    pub(crate) fn flush_usart_to_mgs(&mut self) -> Option<UsartFlush<'_>> {
        // Bail if we don't have any data to flush.
        if !self.needs_usart_flush_to_mgs() {
            return None;
        }

        if let Some((mgs_addr, sp_port)) = self.attached_serial_console_mgs {
            Some(UsartFlush {
                usart: &mut self.usart,
                destination: mgs_addr,
                port: sp_port,
            })
        } else {
            // We have data to flush but no attached MGS instance; discard it.
            self.usart.clear_rx_data();
            None
        }
    }
}

impl SpHandler for MgsHandler {
    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionState { target }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<BulkIgnitionState, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::BulkIgnitionState));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn sp_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<SpState, ResponseError> {
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

    fn update_start(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: UpdateStart,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateStart {
            length: update.total_size
        }));

        if let Some(progress) = self.update_progress.as_ref() {
            return Err(ResponseError::UpdateInProgress {
                bytes_received: progress.bytes_written as u32,
            });
        }

        self.update_task
            .prep_image_update(UpdateTarget::Alternate)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        // We can only call `claim_update_buffer_static` once; we bail out above
        // if `self.update_progress` is already `Some(_)`, and after we claim it
        // here, we store that into `self.update_progress` (and never clear it).
        let update_buffer = claim_update_buffer_static();
        update_buffer.total_length = update.total_size as usize;
        self.update_progress = Some(update_buffer);

        Ok(())
    }

    fn update_chunk(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateChunk {
            offset: chunk.offset,
        }));

        let update_progress = self
            .update_progress
            .as_mut()
            .ok_or(ResponseError::InvalidUpdateChunk)?;

        update_progress.ingest_chunk(&self.update_task, chunk.offset, data)?;

        Ok(())
    }

    fn serial_console_attach(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
        component: SpComponent,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));

        // Including a component in the serial console messages is half-baked at
        // the moment; we can at least check that it's the one component we
        // expect (the host CPU).
        if component != SpComponent::SP3_HOST_CPU {
            return Err(ResponseError::RequestUnsupportedForComponent);
        }

        if self.attached_serial_console_mgs.is_some() {
            return Err(ResponseError::SerialConsoleAlreadyAttached);
        }

        // TODO: Add some kind of auth check before allowing a serial console
        // attach. https://github.com/oxidecomputer/hubris/issues/723
        self.attached_serial_console_mgs = Some((sender, port));
        self.serial_console_write_offset = 0;
        self.usart.from_rx_offset = 0;
        Ok(())
    }

    fn serial_console_write(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
        mut offset: u64,
        mut data: &[u8],
    ) -> Result<u64, ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));

        // TODO: Add some kind of auth check before allowing a serial console
        // attach. https://github.com/oxidecomputer/hubris/issues/723
        //
        // As a temporary measure, we can at least ensure that we only allow
        // writes from the attached console.
        if Some((sender, port)) != self.attached_serial_console_mgs {
            return Err(ResponseError::SerialConsoleNotAttached);
        }

        // Have we already received some or all of this data? (MGS may resend
        // packets that for which it hasn't received our ACK.)
        if self.serial_console_write_offset > offset {
            let skip = self.serial_console_write_offset - offset;
            // Have we already seen _all_ of this data? If so, just reply that
            // we're ready for the data that comes after it.
            if skip >= data.len() as u64 {
                return Ok(offset + data.len() as u64);
            }
            offset = self.serial_console_write_offset;
            data = &data[skip as usize..];
        }

        // Buffer as much of `data` as we can, then notify MGS how much we
        // ingested.
        let can_recv =
            usize::min(self.usart.tx_buffer_remaining_capacity(), data.len());
        self.usart.tx_buffer_append(&data[..can_recv]);
        self.serial_console_write_offset = offset + can_recv as u64;
        Ok(self.serial_console_write_offset)
    }

    fn serial_console_detach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        self.attached_serial_console_mgs = None;
        Ok(())
    }

    fn reset_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SysResetPrepare));
        self.reset_requested = true;
        Ok(())
    }

    fn reset_trigger(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
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
