// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::convert::Infallible;

use crate::{mgs_common::MgsCommon, update::sp::SpUpdate, Log, MgsMessage};
use gateway_messages::{
    sp_impl::SocketAddrV6, sp_impl::SpHandler, BulkIgnitionState,
    ComponentUpdatePrepare, DiscoverResponse, IgnitionCommand, IgnitionState,
    PowerState, ResponseError, SpComponent, SpPort, SpState, SpUpdatePrepare,
    UpdateChunk, UpdateId, UpdateStatus,
};
use ringbuf::ringbuf_entry_root;
use task_net_api::UdpMetadata;
use userlib::sys_get_timer;

// How big does our shared update buffer need to be? Has to be able to handle SP
// update blocks for now, no other updateable components.
const UPDATE_BUFFER_SIZE: usize = SpUpdate::BLOCK_SIZE;

// Create type aliases that include our `UpdateBuffer` size (i.e., the size of
// the largest update chunk of all the components we update).
pub(crate) type UpdateBuffer =
    update_buffer::UpdateBuffer<SpComponent, UPDATE_BUFFER_SIZE>;
pub(crate) type BorrowedUpdateBuffer = update_buffer::BorrowedUpdateBuffer<
    'static,
    SpComponent,
    UPDATE_BUFFER_SIZE,
>;

// Our single, shared update buffer.
static UPDATE_MEMORY: UpdateBuffer = UpdateBuffer::new();

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sp_update: SpUpdate,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            common: MgsCommon::claim_static_resources(),
            sp_update: SpUpdate::new(),
        }
    }

    /// If we want to be woken by the system timer, we return a deadline here.
    /// `main()` is responsible for calling this method and actually setting the
    /// timer.
    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        if self.sp_update.is_preparing() {
            Some(sys_get_timer().now + 1)
        } else {
            None
        }
    }

    pub(crate) fn handle_timer_fired(&mut self) {
        // This is a no-op if we're not preparing for an SP update.
        self.sp_update.step_preparation();
    }

    pub(crate) fn drive_usart(&mut self) {}

    pub(crate) fn wants_to_send_packet_to_mgs(&mut self) -> bool {
        false
    }

    pub(crate) fn packet_to_mgs(
        &mut self,
        _tx_buf: &mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    ) -> Option<UdpMetadata> {
        None
    }
}

impl SpHandler for MgsHandler {
    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        self.common.discover(port)
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionState {
            target
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<BulkIgnitionState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::BulkIgnitionState));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionCommand {
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
        self.common.sp_state()
    }

    fn sp_update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: SpUpdatePrepare,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.aux_flash_size + update.sp_image_size,
            component: SpComponent::SP_ITSELF,
            id: update.id,
            slot: 0,
        }));

        self.sp_update.prepare(&UPDATE_MEMORY, update)
    }

    fn component_update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: ComponentUpdatePrepare,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        // We currently don't have any updateable components on psc.
        Err(ResponseError::RequestUnsupportedForComponent)
    }

    fn update_status(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<UpdateStatus, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateStatus {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => Ok(self.sp_update.status()),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn update_chunk(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
            component: chunk.component,
            offset: chunk.offset,
        }));

        match chunk.component {
            SpComponent::SP_ITSELF | SpComponent::SP_AUX_FLASH => self
                .sp_update
                .ingest_chunk(&chunk.component, &chunk.id, chunk.offset, data),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn update_abort(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
        id: UpdateId,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateAbort {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => self.sp_update.abort(&id),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<PowerState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetPowerState));

        // We have no states other than A2.
        Ok(PowerState::A2)
    }

    fn set_power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        power_state: PowerState,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetPowerState(
            power_state
        )));

        // We have no states other than A2; always fail.
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn serial_console_attach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        _component: SpComponent,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn serial_console_write(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn serial_console_detach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn reset_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        self.common.reset_prepare()
    }

    fn reset_trigger(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<Infallible, ResponseError> {
        self.common.reset_trigger()
    }
}
