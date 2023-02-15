// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::convert::Infallible;

use crate::{
    mgs_common::MgsCommon, update::rot::RotUpdate, update::sp::SpUpdate,
    update::ComponentUpdater, Log, MgsMessage,
};
use gateway_messages::sp_impl::{
    BoundsChecked, DeviceDescription, SocketAddrV6, SpHandler,
};
use gateway_messages::{
    ignition, ComponentDetails, ComponentUpdatePrepare, DiscoverResponse,
    IgnitionCommand, IgnitionState, MgsError, PowerState, SpComponent, SpError,
    SpPort, SpState, SpUpdatePrepare, UpdateChunk, UpdateId, UpdateStatus,
};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_control_plane_agent_api::{ControlPlaneAgentError, VpdIdentity};
use task_net_api::{MacAddress, UdpMetadata};
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
    rot_update: RotUpdate,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            common: MgsCommon::claim_static_resources(base_mac_address),
            sp_update: SpUpdate::new(),
            rot_update: RotUpdate::new(),
        }
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        self.common.identity()
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

    pub(crate) fn fetch_host_phase2_data(
        &mut self,
        _msg: &userlib::RecvMessage,
        _image_hash: [u8; 32],
        _offset: u64,
        _notification_bit: u8,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        Err(ControlPlaneAgentError::DataUnavailable.into())
    }

    pub(crate) fn get_host_phase2_data(
        &mut self,
        _image_hash: [u8; 32],
        _offset: u64,
        _data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        Err(ControlPlaneAgentError::DataUnavailable.into())
    }

    pub(crate) fn startup_options(
        &self,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        // We don't have a host to give startup options; no one should be
        // calling this method.
        Err(ControlPlaneAgentError::InvalidStartupOptions.into())
    }

    pub(crate) fn set_startup_options(
        &mut self,
        _startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        // We don't have a host to give startup options; no one should be
        // calling this method.
        Err(ControlPlaneAgentError::InvalidStartupOptions.into())
    }

    fn power_state_impl(&self) -> Result<PowerState, SpError> {
        // We have no states other than A2.
        Ok(PowerState::A2)
    }
}

impl SpHandler for MgsHandler {
    type BulkIgnitionStateIter = core::iter::Empty<IgnitionState>;
    type BulkIgnitionLinkEventsIter = core::iter::Empty<ignition::LinkEvents>;

    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
        self.common.discover(port)
    }

    fn num_ignition_ports(&mut self) -> Result<u32, SpError> {
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionState { target }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        offset: u32,
    ) -> Result<Self::BulkIgnitionStateIter, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::BulkIgnitionState {
            offset
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_link_events(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<ignition::LinkEvents, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionLinkEvents {
            target
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_link_events(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        offset: u32,
    ) -> Result<Self::BulkIgnitionLinkEventsIter, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::BulkIgnitionLinkEvents {
            offset
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn clear_ignition_link_events(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        _target: Option<u8>,
        _transceiver_select: Option<ignition::TransceiverSelect>,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ClearIgnitionLinkEvents));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn sp_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<SpState, SpError> {
        let power_state = self.power_state_impl()?;
        self.common.sp_state(&self.sp_update, power_state)
    }

    fn sp_update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: SpUpdatePrepare,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdatePrepare {
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
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        match update.component {
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.rot_update.prepare(&UPDATE_MEMORY, update)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_status(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<UpdateStatus, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateStatus { component }));

        match component {
            SpComponent::SP_ITSELF => Ok(self.sp_update.status()),
            SpComponent::ROT | SpComponent::STAGE0 => {
                Ok(self.rot_update.status())
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_chunk(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateChunk {
            component: chunk.component,
            offset: chunk.offset,
        }));

        match chunk.component {
            SpComponent::SP_ITSELF | SpComponent::SP_AUX_FLASH => self
                .sp_update
                .ingest_chunk(&chunk.component, &chunk.id, chunk.offset, data),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.rot_update.ingest_chunk(&chunk.id, chunk.offset, data)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_abort(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
        id: UpdateId,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::UpdateAbort { component }));

        match component {
            SpComponent::SP_ITSELF => self.sp_update.abort(&id),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.rot_update.abort(&id)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<PowerState, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::GetPowerState));
        self.power_state_impl()
    }

    fn set_power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        power_state: PowerState,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SetPowerState(power_state)));

        // We have no states other than A2; always fail.
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_attach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        _component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_write(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_detach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_break(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleBreak));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn reset_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), SpError> {
        self.common.reset_prepare()
    }

    fn reset_trigger(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<Infallible, SpError> {
        self.common.reset_trigger()
    }

    fn num_devices(&mut self, _sender: SocketAddrV6, _port: SpPort) -> u32 {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Inventory));
        self.common.inventory().num_devices() as u32
    }

    fn device_description(
        &mut self,
        index: BoundsChecked,
    ) -> DeviceDescription<'static> {
        self.common.inventory().device_description(index)
    }

    fn num_component_details(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<u32, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ComponentDetails {
            component
        }));

        self.common.inventory().num_component_details(&component)
    }

    fn component_details(
        &mut self,
        component: SpComponent,
        index: BoundsChecked,
    ) -> ComponentDetails {
        self.common.inventory().component_details(&component, index)
    }

    fn component_get_active_slot(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<u16, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ComponentGetActiveSlot {
            component
        }));

        // For now, we don't have any components with active slots.
        match component {
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_set_active_slot(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ComponentSetActiveSlot {
            component,
            slot,
            persist,
        }));

        // For now, we don't have any components with active slots.
        match component {
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_clear_status(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ComponentClearStatus {
            component
        }));
        Err(SpError::RequestUnsupportedForComponent)
    }

    fn get_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::GetStartupOptions));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SetStartupOptions(options)));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn mgs_response_error(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        message_id: u32,
        err: MgsError,
    ) {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::MgsError {
            message_id,
            err
        }));
    }

    fn mgs_response_host_phase2_data(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        _message_id: u32,
        hash: [u8; 32],
        offset: u64,
        data: &[u8],
    ) {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::HostPhase2Data {
            hash,
            offset,
            data_len: data.len(),
        }));
    }

    fn send_host_nmi(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SendHostNmi));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_ipcc_key_lookup_value(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        key: u8,
        value: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SetIpccKeyValue {
            key,
            value_len: value.len(),
        }));
        Err(SpError::RequestUnsupportedForSp)
    }
}
