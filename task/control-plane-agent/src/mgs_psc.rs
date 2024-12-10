// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, update::rot::RotUpdate, update::sp::SpUpdate,
    update::ComponentUpdater, usize_max, CriticalEvent, Log, MgsMessage,
};
use drv_user_leds_api::UserLeds;
use gateway_messages::sp_impl::{
    BoundsChecked, DeviceDescription, Sender, SpHandler,
};
use gateway_messages::{
    ignition, ComponentAction, ComponentActionResponse, ComponentDetails,
    ComponentUpdatePrepare, DiscoverResponse, DumpSegment, DumpTask,
    IgnitionCommand, IgnitionState, MgsError, MgsRequest, MgsResponse,
    PowerState, RotBootInfo, RotRequest, RotResponse, SensorRequest,
    SensorResponse, SpComponent, SpError, SpStateV2, SpUpdatePrepare,
    UpdateChunk, UpdateId, UpdateStatus,
};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::ringbuf_entry_root;
use task_control_plane_agent_api::{ControlPlaneAgentError, VpdIdentity};
use task_net_api::{MacAddress, UdpMetadata, VLanId};
use userlib::sys_get_timer;

// How big does our shared update buffer need to be? Has to be able to handle SP
// update blocks for now, no other updateable components.
const UPDATE_BUFFER_SIZE: usize =
    usize_max(SpUpdate::BLOCK_SIZE, RotUpdate::BLOCK_SIZE);

userlib::task_slot!(USER_LEDS, user_leds);

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
    user_leds: UserLeds,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            common: MgsCommon::claim_static_resources(base_mac_address),
            user_leds: UserLeds::from(USER_LEDS.get_task_id()),
        }
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        self.common.identity()
    }

    /// If we want to be woken by the system timer, we return a deadline here.
    /// `main()` is responsible for calling this method and actually setting the
    /// timer.
    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        if self.common.sp_update.is_preparing() {
            Some(sys_get_timer().now + 1)
        } else {
            None
        }
    }

    pub(crate) fn handle_timer_fired(&mut self) {
        // This is a no-op if we're not preparing for an SP update.
        self.common.sp_update.step_preparation();
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

    pub(crate) fn startup_options_impl(
        &self,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        // We don't have a host to give startup options; no one should be
        // calling this method.
        Err(ControlPlaneAgentError::InvalidStartupOptions.into())
    }

    pub(crate) fn set_startup_options_impl(
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
    type VLanId = VLanId;

    fn ensure_request_trusted(
        &mut self,
        kind: MgsRequest,
        _sender: Sender<VLanId>,
    ) -> Result<MgsRequest, SpError> {
        // PSCs are okay with everyone talking to them, since they're behind the
        // management network.
        Ok(kind)
    }

    fn ensure_response_trusted(
        &mut self,
        kind: MgsResponse,
        _sender: Sender<VLanId>,
    ) -> Option<MgsResponse> {
        // PSCs are okay with everyone talking to them, since they're behind
        // the management network.
        Some(kind)
    }

    fn discover(
        &mut self,
        sender: Sender<VLanId>,
    ) -> Result<DiscoverResponse, SpError> {
        self.common.discover(sender.vid)
    }

    fn num_ignition_ports(&mut self) -> Result<u32, SpError> {
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_state(&mut self, target: u8) -> Result<IgnitionState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionState {
            target
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        offset: u32,
    ) -> Result<Self::BulkIgnitionStateIter, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::BulkIgnitionState {
            offset
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_link_events(
        &mut self,
        target: u8,
    ) -> Result<ignition::LinkEvents, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionLinkEvents {
            target
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_link_events(
        &mut self,
        offset: u32,
    ) -> Result<Self::BulkIgnitionLinkEventsIter, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::BulkIgnitionLinkEvents { offset }
        ));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn clear_ignition_link_events(
        &mut self,
        _target: Option<u8>,
        _transceiver_select: Option<ignition::TransceiverSelect>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ClearIgnitionLinkEvents
        ));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn sp_state(&mut self) -> Result<SpStateV2, SpError> {
        let power_state = self.power_state_impl()?;
        self.common.sp_state(power_state)
    }

    fn sp_update_prepare(
        &mut self,
        update: SpUpdatePrepare,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.aux_flash_size + update.sp_image_size,
            component: SpComponent::SP_ITSELF,
            id: update.id,
            slot: 0,
        }));

        self.common.sp_update.prepare(&UPDATE_MEMORY, update)
    }

    fn component_update_prepare(
        &mut self,
        update: ComponentUpdatePrepare,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        match update.component {
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.common.rot_update.prepare(&UPDATE_MEMORY, update)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_action(
        &mut self,
        _sender: Sender<VLanId>,
        component: SpComponent,
        action: ComponentAction,
    ) -> Result<ComponentActionResponse, SpError> {
        match (component, action) {
            (SpComponent::SYSTEM_LED, ComponentAction::Led(action)) => {
                use gateway_messages::LedComponentAction;
                // Setting the LED should be infallible, because we know that
                // this board supports LED 0 as the system LED.
                match action {
                    LedComponentAction::TurnOn => self.user_leds.led_on(0),
                    LedComponentAction::TurnOff => self.user_leds.led_off(0),
                    LedComponentAction::Blink => self.user_leds.led_blink(0),
                }
                .unwrap();
                Ok(ComponentActionResponse::Ack)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_status(
        &mut self,
        component: SpComponent,
    ) -> Result<UpdateStatus, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateStatus {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => Ok(self.common.sp_update.status()),
            SpComponent::ROT | SpComponent::STAGE0 => {
                Ok(self.common.rot_update.status())
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
            component: chunk.component,
            offset: chunk.offset,
        }));

        match chunk.component {
            SpComponent::SP_ITSELF | SpComponent::SP_AUX_FLASH => self
                .common
                .sp_update
                .ingest_chunk(&chunk.component, &chunk.id, chunk.offset, data),
            SpComponent::ROT | SpComponent::STAGE0 => self
                .common
                .rot_update
                .ingest_chunk(&(), &chunk.id, chunk.offset, data),
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_abort(
        &mut self,
        component: SpComponent,
        id: UpdateId,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateAbort {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => self.common.sp_update.abort(&id),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.common.rot_update.abort(&id)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(&mut self) -> Result<PowerState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetPowerState));
        self.power_state_impl()
    }

    fn set_power_state(
        &mut self,
        sender: Sender<VLanId>,
        power_state: PowerState,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(
            CRITICAL,
            CriticalEvent::SetPowerState {
                sender,
                power_state,
                ticks_since_boot: sys_get_timer().now,
            }
        );
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetPowerState(
            power_state
        )));

        // We have no states other than A2; always fail.
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_attach(
        &mut self,
        _sender: Sender<VLanId>,
        _component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_write(
        &mut self,
        _sender: Sender<VLanId>,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_keepalive(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::SerialConsoleKeepAlive
        ));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_detach(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_break(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleBreak));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn num_devices(&mut self) -> u32 {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::Inventory));
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
        component: SpComponent,
    ) -> Result<u32, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::ComponentDetails {
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
        component: SpComponent,
    ) -> Result<u16, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentGetActiveSlot { component }
        ));

        self.common.component_get_active_slot(component)
    }

    fn component_set_active_slot(
        &mut self,
        component: SpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentSetActiveSlot {
                component,
                slot,
                persist,
            }
        ));

        self.common
            .component_set_active_slot(component, slot, persist)
    }

    fn component_clear_status(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentClearStatus { component }
        ));
        Err(SpError::RequestUnsupportedForComponent)
    }

    fn get_startup_options(
        &mut self,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetStartupOptions));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_startup_options(
        &mut self,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetStartupOptions(
            options
        )));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn mgs_response_error(&mut self, message_id: u32, err: MgsError) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::MgsError {
            message_id,
            err
        }));
    }

    fn mgs_response_host_phase2_data(
        &mut self,
        _sender: Sender<VLanId>,
        _message_id: u32,
        hash: [u8; 32],
        offset: u64,
        data: &[u8],
    ) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::HostPhase2Data {
            hash,
            offset,
            data_len: data.len(),
        }));
    }

    fn send_host_nmi(&mut self) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SendHostNmi));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_ipcc_key_lookup_value(
        &mut self,
        key: u8,
        value: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetIpccKeyValue {
            key,
            value_len: value.len(),
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn get_component_caboose_value(
        &mut self,
        component: SpComponent,
        slot: u16,
        key: [u8; 4],
        buf: &mut [u8],
    ) -> Result<usize, SpError> {
        self.common
            .get_component_caboose_value(component, slot, key, buf)
    }

    fn reset_component_prepare(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.reset_component_prepare(component)
    }

    fn reset_component_trigger(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.reset_component_trigger(component)
    }

    fn read_sensor(
        &mut self,
        req: SensorRequest,
    ) -> Result<SensorResponse, SpError> {
        self.common.read_sensor(req)
    }

    fn current_time(&mut self) -> Result<u64, SpError> {
        self.common.current_time()
    }

    fn read_rot(
        &mut self,
        req: RotRequest,
        buf: &mut [u8],
    ) -> Result<RotResponse, SpError> {
        self.common.read_rot_page(req, buf)
    }

    fn vpd_lock_status_all(
        &mut self,
        buf: &mut [u8],
    ) -> Result<usize, SpError> {
        self.common.vpd_lock_status_all(buf)
    }

    fn reset_component_trigger_with_watchdog(
        &mut self,
        component: SpComponent,
        time_ms: u32,
    ) -> Result<(), SpError> {
        self.common
            .reset_component_trigger_with_watchdog(component, time_ms)
    }

    fn disable_component_watchdog(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.disable_component_watchdog(component)
    }

    fn component_watchdog_supported(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.component_watchdog_supported(component)
    }

    fn versioned_rot_boot_info(
        &mut self,
        version: u8,
    ) -> Result<RotBootInfo, SpError> {
        self.common.versioned_rot_boot_info(version)
    }

    fn get_task_dump_count(&mut self) -> Result<u32, SpError> {
        self.common.get_task_dump_count()
    }

    fn task_dump_read_start(
        &mut self,
        index: u32,
        key: [u8; 16],
    ) -> Result<DumpTask, SpError> {
        self.common.task_dump_read_start(index, key)
    }

    fn task_dump_read_continue(
        &mut self,
        key: [u8; 16],
        seq: u32,
        buf: &mut [u8],
    ) -> Result<Option<DumpSegment>, SpError> {
        self.common.task_dump_read_continue(key, seq, buf)
    }
}
