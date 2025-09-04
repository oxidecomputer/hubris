// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, notifications, update::host_flash::HostFlashUpdate,
    update::rot::RotUpdate, update::sp::SpUpdate, update::ComponentUpdater,
    usize_max, CriticalEvent, Log, MgsMessage, SYS,
};
use core::time::Duration;
use drv_cpu_seq_api::Sequencer;
use drv_stm32h7_usart::Usart;
use drv_user_leds_api::UserLeds;
use gateway_messages::sp_impl::{
    BoundsChecked, DeviceDescription, Sender, SpHandler,
};
use gateway_messages::{
    ignition, ComponentAction, ComponentActionResponse, ComponentDetails,
    ComponentUpdatePrepare, DiscoverResponse, DumpSegment, DumpTask, Header,
    IgnitionCommand, IgnitionState, Message, MessageKind, MgsError, MgsRequest,
    MgsResponse, PowerState, PowerStateTransition, RotBootInfo, RotRequest,
    RotResponse, SensorRequest, SensorResponse, SpComponent, SpError,
    SpPort as GwSpPort, SpRequest, SpStateV2, SpUpdatePrepare, UpdateChunk,
    UpdateId, UpdateStatus, SERIAL_CONSOLE_IDLE_TIMEOUT,
};
use heapless::{Deque, Vec};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::ringbuf_entry_root;
use static_cell::ClaimOnceCell;
use task_control_plane_agent_api::{
    ControlPlaneAgentError, UartClient, VpdIdentity,
    MAX_INSTALLINATOR_IMAGE_ID_LEN,
};
use task_net_api::{Address, MacAddress, UdpMetadata, VLanId};
use userlib::{sys_get_timer, sys_irq_control, FromPrimitive, UnwrapLite};

// We're included under a special `path` cfg from main.rs, which confuses rustc
// about where our submodules live. Pass explicit paths to correct it.
#[path = "mgs_compute_sled/host_phase2.rs"]
mod host_phase2;

use host_phase2::HostPhase2Requester;

// How big does our shared update buffer need to be? Has to be able to handle SP
// update blocks or host flash pages.
const UPDATE_BUFFER_SIZE: usize = usize_max(
    usize_max(SpUpdate::BLOCK_SIZE, HostFlashUpdate::BLOCK_SIZE),
    RotUpdate::BLOCK_SIZE,
);

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

/// Buffer sizes for serial console UDP / USART proxying.
///
/// MGS -> SP should be at least as large as the amount of data we can receive
/// in a single packet; otherwise MGS will have to resend data in subsequent
/// packets.
///
/// SP -> MGS can be whatever size we want, but the larger it is the less likely
/// we are to lose data while waiting to flush from our buffer out to UDP. We'll
/// start flushing once we cross SP_TO_MGS_SERIAL_CONSOLE_FLUSH_WATERMARK.
const MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE: usize =
    gateway_messages::MAX_SERIALIZED_SIZE;
const SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE: usize = 4096;
const SP_TO_MGS_SERIAL_CONSOLE_FLUSH_WATERMARK: usize =
    gateway_messages::MAX_SERIALIZED_SIZE;

static_assertions::const_assert!(
    SP_TO_MGS_SERIAL_CONSOLE_FLUSH_WATERMARK
        <= SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE
);

/// Send any buffered serial console data to MGS when our oldest buffered byte
/// is this old, even if our buffer isn't full yet.
const SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS: u64 = 500;

userlib::task_slot!(HOST_FLASH, hf);
userlib::task_slot!(CPU_SEQ, cpu_seq);
userlib::task_slot!(USER_LEDS, user_leds);

type InstallinatorImageIdBuf = Vec<u8, MAX_INSTALLINATOR_IMAGE_ID_LEN>;

struct AttachedSerialConsoleMgs {
    sender: Sender<VLanId>,
    // The timestamp of the most recent keepalive (which can be an actual
    // keepalive packet or any other meaningful serial-console-related message:
    // connection, write, break, keepalive).
    last_keepalive_received: u64, // from sys_get_timer().now
}

impl AttachedSerialConsoleMgs {
    /// If `sender` and `port` match `self.address` and `self.port`, updates
    /// `self.last_keepalive_received` to `sys_get_timer().now`. Otherwise,
    /// returns an error.
    fn check_sender_and_update_keepalive(
        &mut self,
        sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        if sender != self.sender {
            return Err(SpError::SerialConsoleNotAttached);
        }

        self.last_keepalive_received = sys_get_timer().now;
        Ok(())
    }
}

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sequencer: Sequencer,
    host_flash_update: HostFlashUpdate,
    host_phase2: HostPhase2Requester,
    usart: UsartHandler,
    user_leds: UserLeds,
    attached_serial_console_mgs: Option<AttachedSerialConsoleMgs>,
    serial_console_write_offset: u64,
    next_message_id: u32,
    installinator_image_id: &'static mut InstallinatorImageIdBuf,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        struct Bufs {
            usart_to_tx: Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE>,
            usart_from_rx: Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE>,
            installinator_image_id: InstallinatorImageIdBuf,
            host_phase2_buf: host_phase2::Phase2Buf,
        }
        let Bufs {
            ref mut usart_to_tx,
            ref mut usart_from_rx,
            ref mut installinator_image_id,
            ref mut host_phase2_buf,
        } = {
            static BUFS: ClaimOnceCell<Bufs> = ClaimOnceCell::new(Bufs {
                usart_to_tx: Deque::new(),
                usart_from_rx: Deque::new(),
                host_phase2_buf: host_phase2::Phase2Buf::new(),
                installinator_image_id: InstallinatorImageIdBuf::new(),
            });
            BUFS.claim()
        };
        let usart = UsartHandler::new(usart_to_tx, usart_from_rx);

        Self {
            common: MgsCommon::claim_static_resources(base_mac_address),
            host_flash_update: HostFlashUpdate::new(),
            host_phase2: HostPhase2Requester::new(host_phase2_buf),
            sequencer: Sequencer::from(CPU_SEQ.get_task_id()),
            user_leds: UserLeds::from(USER_LEDS.get_task_id()),
            usart,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
            next_message_id: 0,
            installinator_image_id,
        }
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        self.common.identity()
    }

    pub(crate) fn installinator_image_id(&self) -> &[u8] {
        self.installinator_image_id
    }

    /// If we want to be woken by the system timer, we return a deadline here.
    /// `main()` is responsible for calling this method and actually setting the
    /// timer.
    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        // If we're trying to prep for a host flash update, we have sectors that
        // need to be erased, but we break that work up across multiple steps to
        // avoid blocking while the entire erase happens. If we're in that case,
        // set our timer for 1 tick from now to give a window for other
        // interrupts/notifications to arrive.
        if self.host_flash_update.is_preparing()
            || self.common.sp_update.is_preparing()
        {
            Some(sys_get_timer().now + 1)
        } else {
            match (
                self.usart.from_rx_flush_deadline,
                self.host_phase2.timer_deadline(),
            ) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
    }

    pub(crate) fn handle_timer_fired(&mut self) {
        // We use a shared update buffer, so at most one of these updates can be
        // active at a time. For any inactive update, `step_preparation()` is a
        // no-op.
        self.host_flash_update.step_preparation();
        self.common.sp_update.step_preparation();
        // Even though `timer_deadline()` can return a timer related to usart
        // flushing or host phase2 data handling, we don't need to do anything
        // here; `NetHandler` in main.rs will call
        // `wants_to_send_packet_to_mgs()` below when it's ready to grab any
        // data we want to send.
    }

    pub(crate) fn uart_client(&self) -> UartClient {
        self.usart.client
    }

    pub(crate) fn set_uart_client(
        &mut self,
        client: UartClient,
    ) -> Result<(), ControlPlaneAgentError> {
        // Refuse to switch to humility if MGS is currently attached.
        if client == UartClient::Humility
            && self.attached_serial_console_mgs.is_some()
        {
            return Err(ControlPlaneAgentError::MgsAttachedToUart);
        }

        self.usart.set_client(client);
        Ok(())
    }

    pub(crate) fn drive_usart(&mut self) {
        self.usart.run_until_blocked();
    }

    pub(crate) fn wants_to_send_packet_to_mgs(&mut self) -> bool {
        // If we should be forwarding uart data to MGS but we don't have one
        // attached, discard any buffered data.
        if self.usart.client == UartClient::Mgs
            && self.attached_serial_console_mgs.is_none()
        {
            self.usart.clear_rx_data();
        }

        self.usart.should_flush_to_mgs()
            || self.host_phase2.wants_to_send_packet()
    }

    fn next_message_id(&mut self) -> u32 {
        let id = self.next_message_id;
        self.next_message_id = id.wrapping_add(1);
        id
    }

    pub(crate) fn packet_to_mgs(
        &mut self,
        tx_buf: &mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    ) -> Option<UdpMetadata> {
        // Do we need to request host phase2 data?
        if self.host_phase2.wants_to_send_packet() {
            let message_id = self.next_message_id();
            if let Some(meta) =
                self.host_phase2.packet_to_mgs(message_id, tx_buf)
            {
                return Some(meta);
            }
        }

        // Should we flush any buffered usart data out to MGS?
        if !self.usart.should_flush_to_mgs() {
            return None;
        }

        // Do we have an attached MGS instance that hasn't gone stale?
        let sender = match &self.attached_serial_console_mgs {
            Some(attached) => {
                // Check whether we think this client has disappeared
                let client_age_ms = sys_get_timer()
                    .now
                    .saturating_sub(attached.last_keepalive_received);
                if Duration::from_millis(client_age_ms)
                    > SERIAL_CONSOLE_IDLE_TIMEOUT
                {
                    self.usart.clear_rx_data();
                    self.attached_serial_console_mgs = None;
                    return None;
                }
                attached.sender
            }
            None => {
                // Discard any buffered data and reset any usart-related timers.
                self.usart.clear_rx_data();
                return None;
            }
        };

        // We have data we want to flush and an attached MGS; build our packet.
        ringbuf_entry_root!(Log::SerialConsoleSend {
            buffered: self.usart.from_rx.len(),
        });

        let message = Message {
            header: Header {
                version: gateway_messages::version::CURRENT,
                message_id: self.next_message_id(),
            },
            kind: MessageKind::SpRequest(SpRequest::SerialConsole {
                component: SpComponent::SP3_HOST_CPU,
                offset: self.usart.from_rx_offset,
            }),
        };

        let (from_rx0, from_rx1) = self.usart.from_rx.as_slices();
        let (n, written) = gateway_messages::serialize_with_trailing_data(
            tx_buf,
            &message,
            &[from_rx0, from_rx1],
        );

        // Note: We do not wait for an ack from MGS after sending this data; we
        // hope it receives it, but if not, it's lost. We don't have the buffer
        // space to keep a bunch of data around waiting for acks, and in
        // practice we don't expect lost packets to be a problem.
        self.usart.drain_flushed_data(written);

        Some(UdpMetadata {
            addr: Address::Ipv6(sender.addr.ip.into()),
            port: sender.addr.port,
            size: n as u32,
            vid: sender.vid,
        })
    }

    pub(crate) fn uart_read(
        &mut self,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        // This function is only called by humility; switch control to it.
        self.set_uart_client(UartClient::Humility)?;

        let mut i = 0;
        while i < data.len() {
            let Some(b) = self.usart.from_rx.pop_front() else {
                break;
            };

            data.write_at(i, b)
                .map_err(|()| RequestError::went_away())?;

            i += 1;
        }

        if !self.usart.from_rx.is_full() {
            // If `from_rx` was full and our client was set to Humility the last
            // time we handled a uart interrupt, we disabled the rx interrupt.
            // Re-enable it now that humility has pulled some data from us and
            // made space for more.
            self.usart.usart.enable_rx_interrupt();
        }

        Ok(i)
    }

    pub(crate) fn uart_write(
        &mut self,
        data: Leased<idol_runtime::R, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        const CHUNK_SIZE: usize = 32;

        // This function is only called by humility; switch control to it.
        self.set_uart_client(UartClient::Humility)?;

        let mut chunk = [0; CHUNK_SIZE];
        let mut i = 0;

        while self.usart.tx_buffer_remaining_capacity() > 0 && i < data.len() {
            // Min of all three: remaining buffer, remaining data, sizeof(chunk)
            let n = usize::min(
                self.usart.tx_buffer_remaining_capacity(),
                usize::min(CHUNK_SIZE, data.len() - i),
            );
            data.read_range(i..i + n, &mut chunk[..n])
                .map_err(|()| RequestError::went_away())?;
            self.usart.tx_buffer_append(&chunk[..n]);
            i += n;
        }

        Ok(i)
    }

    pub(crate) fn fetch_host_phase2_data(
        &mut self,
        msg: &userlib::RecvMessage,
        image_hash: [u8; 32],
        offset: u64,
        notification_bit: u8,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        self.host_phase2.start_fetch(
            msg.sender,
            notification_bit,
            image_hash,
            offset,
        );
        Ok(())
    }

    pub(crate) fn get_host_phase2_data(
        &mut self,
        image_hash: [u8; 32],
        offset: u64,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        self.host_phase2.get_data(image_hash, offset, data)
    }

    pub(crate) fn startup_options_impl(
        &self,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        Ok(self.common.packrat().get_next_boot_host_startup_options())
    }

    pub(crate) fn set_startup_options_impl(
        &mut self,
        startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        self.common
            .packrat()
            .set_next_boot_host_startup_options(startup_options);
        Ok(())
    }

    fn power_state_impl(&self) -> Result<PowerState, SpError> {
        use drv_cpu_seq_api::PowerState as DrvPowerState;

        // TODO Do we want to expose the sub-states to the control plane? For
        // now, squish them down.
        //
        // TODO Do we want to expose A1 to the control plane at all? If not,
        // what would we map it to? Maybe easier to leave it exposed.
        let state = match self.sequencer.get_state() {
            DrvPowerState::A2 | DrvPowerState::A2PlusFans => PowerState::A2,
            DrvPowerState::A0
            | DrvPowerState::A0PlusHP
            | DrvPowerState::A0Thermtrip
            | DrvPowerState::A0Reset => PowerState::A0,
        };

        Ok(state)
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
        // Gimlets are okay with everyone talking to them, since they're behind
        // the management network.
        Ok(kind)
    }

    fn ensure_response_trusted(
        &mut self,
        kind: MgsResponse,
        _sender: Sender<VLanId>,
    ) -> Option<MgsResponse> {
        // Gimlets are okay with everyone talking to them, since they're behind
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
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.prepare(&UPDATE_MEMORY, update)
            }
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
            SpComponent::HOST_CPU_BOOT_FLASH => self
                .host_flash_update
                .ingest_chunk(&(), &chunk.id, chunk.offset, data),
            SpComponent::ROT | SpComponent::STAGE0 => self
                .common
                .rot_update
                .ingest_chunk(&(), &chunk.id, chunk.offset, data),
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

        let status = match component {
            // Unlike `update_chunk()`, we only need to match on `SP_ITSELF`
            // here and not `SP_AUX_FLASH`. We mostly hide the fact that an SP
            // update also may include an aux flash image from clients: when
            // they start an update, it is for `SP_ITSELF` (whether or not it
            // includes an aux flash image, which they don't need to know).
            // Similarly, they will only ask for the status of an `SP_ITSELF`
            // update, not an `SP_AUX_FLASH` update (which isn't a thing).
            SpComponent::SP_ITSELF => self.common.sp_update.status(),
            SpComponent::HOST_CPU_BOOT_FLASH => self.host_flash_update.status(),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.common.rot_update.status()
            }
            _ => return Err(SpError::RequestUnsupportedForComponent),
        };

        Ok(status)
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
            // Unlike `update_chunk()`, we only need to match on `SP_ITSELF`
            // here and not `SP_AUX_FLASH`. We mostly hide the fact that an SP
            // update also may include an aux flash image from clients: when
            // they start an update, it is for `SP_ITSELF` (whether or not it
            // includes an aux flash image, which they don't need to know).
            // Similarly, they will only attempt to abort an `SP_ITSELF`
            // update, not an `SP_AUX_FLASH` update (which isn't a thing).
            SpComponent::SP_ITSELF => self.common.sp_update.abort(&id),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.abort(&id)
            }
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
    ) -> Result<PowerStateTransition, SpError> {
        use drv_cpu_seq_api::PowerState as DrvPowerState;
        use drv_cpu_seq_api::Transition;

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

        let power_state = match power_state {
            PowerState::A0 => DrvPowerState::A0,
            // Nothing should every try to go into A1
            PowerState::A1 => {
                return Err(SpError::PowerStateError(
                    drv_cpu_seq_api::SeqError::IllegalTransition.into(),
                ))
            }
            PowerState::A2 => DrvPowerState::A2,
        };

        let transition = self
            .sequencer
            .set_state_with_reason(
                power_state,
                drv_cpu_seq_api::StateChangeReason::ControlPlane,
            )
            .map_err(|e| SpError::PowerStateError(e as u32))?;

        Ok(match transition {
            Transition::Changed => PowerStateTransition::Changed,
            Transition::Unchanged => PowerStateTransition::Unchanged,
        })
    }

    fn serial_console_attach(
        &mut self,
        sender: Sender<VLanId>,
        component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));

        // Including a component in the serial console messages is half-baked at
        // the moment; we can at least check that it's the one component we
        // expect (the host CPU).
        if component != SpComponent::SP3_HOST_CPU {
            return Err(SpError::RequestUnsupportedForComponent);
        }

        if self.attached_serial_console_mgs.is_some() {
            return Err(SpError::SerialConsoleAlreadyAttached);
        }

        // TODO: Add some kind of auth check before allowing a serial console
        // attach. https://github.com/oxidecomputer/hubris/issues/723
        self.attached_serial_console_mgs = Some(AttachedSerialConsoleMgs {
            sender,
            last_keepalive_received: sys_get_timer().now,
        });
        self.serial_console_write_offset = 0;
        self.usart.from_rx_offset = 0;

        // Forcibly setting the client to MGS here will disconnect any active
        // humility connections the next time they poll us or send us data to
        // write, which seems fine: If MGS is available, we probably don't have
        // a dongle attached to even use humility.
        self.usart.set_client(UartClient::Mgs);

        Ok(())
    }

    fn serial_console_write(
        &mut self,
        sender: Sender<VLanId>,
        mut offset: u64,
        mut data: &[u8],
    ) -> Result<u64, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));

        // TODO: Add some kind of auth check before allowing a serial console
        // attach. https://github.com/oxidecomputer/hubris/issues/723
        //
        // As a temporary measure, we can at least ensure that we only allow
        // writes from the attached console.
        self.attached_serial_console_mgs
            .as_mut()
            .ok_or(SpError::SerialConsoleNotAttached)?
            .check_sender_and_update_keepalive(sender)?;

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

    fn serial_console_keepalive(
        &mut self,
        sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::SerialConsoleKeepAlive
        ));
        self.attached_serial_console_mgs
            .as_mut()
            .ok_or(SpError::SerialConsoleNotAttached)?
            .check_sender_and_update_keepalive(sender)?;
        Ok(())
    }

    fn serial_console_detach(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        self.attached_serial_console_mgs = None;
        Ok(())
    }

    fn serial_console_break(
        &mut self,
        sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleBreak));
        // TODO: same caveats as above!
        self.attached_serial_console_mgs
            .as_mut()
            .ok_or(SpError::SerialConsoleNotAttached)?
            .check_sender_and_update_keepalive(sender)?;
        self.usart.send_break();
        Ok(())
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

    fn get_startup_options(
        &mut self,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetStartupOptions));

        // Our `startup_options_impl` never fails, so is safe to unwrap.
        Ok(self.startup_options_impl().unwrap_lite().into())
    }

    fn set_startup_options(
        &mut self,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetStartupOptions(
            options
        )));

        // Our `set_startup_options_impl` never fails, so is safe to unwrap.
        self.set_startup_options_impl(options.into()).unwrap_lite();

        Ok(())
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

        match component {
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.active_slot()
            }
            _ => self.common.component_get_active_slot(component),
        }
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
        match component {
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.set_active_slot(slot, persist)
            }
            _ => self
                .common
                .component_set_active_slot(component, slot, persist),
        }
    }

    fn component_cancel_pending_active_slot(
        &mut self,
        component: SpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentCancelPendingActiveSlot {
                component,
                slot,
                persist,
            }
        ));
        match component {
            SpComponent::HOST_CPU_BOOT_FLASH => {
                Err(SpError::RequestUnsupportedForComponent)
            }
            _ => self
                .common
                .component_cancel_pending_active_slot(component, slot, persist),
        }
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

    fn mgs_response_error(&mut self, message_id: u32, err: MgsError) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::MgsError {
            message_id,
            err
        }));
    }

    fn mgs_response_host_phase2_data(
        &mut self,
        sender: Sender<VLanId>,
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

        self.host_phase2.ingest_incoming_data(
            match sender.vid.cfg().port {
                task_net_api::SpPort::One => GwSpPort::One,
                task_net_api::SpPort::Two => GwSpPort::Two,
            },
            hash,
            offset,
            data,
        );
    }

    fn send_host_nmi(&mut self) -> Result<(), SpError> {
        // This can only fail if the `gimlet-seq` server is dead; in that
        // case, send `Busy` because it should be rebooting.
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SendHostNmi));
        self.sequencer
            .send_hardware_nmi()
            .map_err(|_| SpError::Busy)?;
        Ok(())
    }

    fn set_ipcc_key_lookup_value(
        &mut self,
        key: u8,
        value: &[u8],
    ) -> Result<(), SpError> {
        use gateway_messages::IpccKeyLookupValueError;
        use host_sp_messages::Key;

        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetIpccKeyValue {
            key,
            value_len: value.len(),
        }));

        match Key::from_u8(key) {
            Some(Key::InstallinatorImageId) => {
                // Check the incoming data length first; if this fails, we'll
                // keep whatever existing image ID we have.
                let max_len = self.installinator_image_id.capacity();
                if value.len() > max_len {
                    return Err(SpError::SetIpccKeyLookupValueFailed(
                        IpccKeyLookupValueError::ValueTooLong {
                            max_len: max_len as u16,
                        },
                    ));
                }

                // We now know `value` will fit, so replace our current
                // installinator ID and unwrap the `extend_from_slice()`.
                self.installinator_image_id.clear();
                self.installinator_image_id
                    .extend_from_slice(value)
                    .unwrap_lite();
                Ok(())
            }
            Some(Key::Ping)
            | Some(Key::InventorySize)
            | Some(Key::EtcSystem)
            | Some(Key::DtraceConf)
            | None => Err(SpError::SetIpccKeyLookupValueFailed(
                IpccKeyLookupValueError::InvalidKey,
            )),
        }
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
            .map(|_| ())
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

    fn read_host_flash(
        &mut self,
        slot: u16,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::ReadHostFlash {
            addr
        }));
        self.host_flash_update.read_page(slot, addr, buf)
    }

    fn start_host_flash_hash(&mut self, slot: u16) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::StartHostFlashHash {
            slot
        }));
        self.host_flash_update.start_hash(slot)
    }

    fn get_host_flash_hash(&mut self, slot: u16) -> Result<[u8; 32], SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetHostFlashHash {
            slot
        }));
        self.host_flash_update.get_hash(slot)
    }
}

struct UsartHandler {
    usart: Usart,
    to_tx: &'static mut Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE>,
    from_rx: &'static mut Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE>,
    from_rx_flush_deadline: Option<u64>,
    from_rx_offset: u64,
    client: UartClient,
}

impl UsartHandler {
    fn new(
        to_tx: &'static mut Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE>,
        from_rx: &'static mut Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE>,
    ) -> Self {
        let usart = configure_usart();

        // Enable USART interrupts.
        sys_irq_control(notifications::USART_IRQ_MASK, true);

        Self {
            usart,
            to_tx,
            from_rx,
            from_rx_flush_deadline: None,
            from_rx_offset: 0,
            client: UartClient::Mgs,
        }
    }

    fn tx_buffer_remaining_capacity(&self) -> usize {
        self.to_tx.capacity() - self.to_tx.len()
    }

    /// Panics if `self.tx_buffer_remaining_capacity() < data.len()`.
    fn tx_buffer_append(&mut self, data: &[u8]) {
        if self.to_tx.is_empty() {
            self.usart.enable_tx_fifo_empty_interrupt();
        }
        for &b in data {
            self.to_tx.push_back(b).unwrap_lite();
        }
    }

    fn set_client(&mut self, client: UartClient) {
        self.client = client;

        match client {
            UartClient::Humility => {
                // We never flush to humility; it polls us.
                self.from_rx_flush_deadline = None;
            }
            UartClient::Mgs => {
                // Humility might've disabled the rx interrupt if our rx buffer
                // filled; re-enable it.
                self.usart.enable_rx_interrupt();
            }
        }
    }

    fn should_flush_to_mgs(&self) -> bool {
        // If we're configured to speak to humility, we never flush to MGS.
        if self.client == UartClient::Humility {
            return false;
        }

        // Bail out early if our buffer is empty or past the "we should flush"
        // watermark.
        let len = self.from_rx.len();
        if len == 0 {
            return false;
        } else if len >= SP_TO_MGS_SERIAL_CONSOLE_FLUSH_WATERMARK {
            return true;
        }

        // Otherwise, only flush if we're past our deadline.
        self.from_rx_flush_deadline
            .map(|deadline| sys_get_timer().now >= deadline)
            .unwrap_or(false)
    }

    fn clear_rx_data(&mut self) {
        self.from_rx.clear();
        self.from_rx_flush_deadline = None;
    }

    fn drain_flushed_data(&mut self, n: usize) {
        self.from_rx.drain_front(n);
        self.from_rx_offset += n as u64;
        self.from_rx_flush_deadline = None;
        if !self.from_rx.is_empty() {
            self.set_from_rx_flush_deadline();
        }
    }

    /// Panics if `self.from_rx_deadline.is_some()` or if
    /// `self.from_rx.is_empty()`; callers are responsible for checking or
    /// ensuring both.
    fn set_from_rx_flush_deadline(&mut self) {
        // If we're configured to speak to humility, we never flush to MGS.
        if self.client == UartClient::Humility {
            return;
        }

        assert!(self.from_rx_flush_deadline.is_none());
        assert!(!self.from_rx.is_empty());
        let deadline =
            sys_get_timer().now + SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS;
        self.from_rx_flush_deadline = Some(deadline);
    }

    fn run_until_blocked(&mut self) {
        // Transmit as much as we have and can.
        let mut n_transmitted = 0;
        for &b in &*self.to_tx {
            if self.usart.try_tx_push(b) {
                n_transmitted += 1;
            } else {
                break;
            }
        }

        // Drain the data we successfully transmitted.
        if n_transmitted > 0 {
            self.to_tx.drain_front(n_transmitted);
        }

        // Either disable the tx fifo interrupt (if we have no data left to
        // send) or ringbuf-log that we filled the fifo.
        if self.to_tx.is_empty() {
            self.usart.disable_tx_fifo_empty_interrupt();
        } else {
            ringbuf_entry_root!(Log::UsartTxFull {
                remaining: self.to_tx.len()
            });
        }

        // Clear any errors.
        if self.usart.check_and_clear_rx_overrun() {
            ringbuf_entry_root!(Log::UsartRxOverrun);
            // TODO-correctness Should we notify MGS of dropped data here? We
            // could increment `self.from_rx_offset`, but (a) we don't know how
            // much data we lost, and (b) it would indicate lost data in the
            // wrong place (i.e., data lost at the current
            // `self.from_rx_offset`, instead of `self.from_rx_offset +
            // self.from_rx.len()`, which is where we actually are now).
        }

        let mut n_received = 0;
        let mut discarded_data = 0;

        // If our client is Humility, we only want to read from the usart if we
        // have room in our buffer (i.e., we want to make use of flow control
        // because Humility is a _very slow_ reader). By refusing to read from
        // the RX FIFO in such a case, we'll get flow control (because we enable
        // hardware flow control, which is automatic based on the FIFO
        // fullness).
        //
        // If our client is MGS (the default), we always recv as much as we can
        // from the USART FIFO, even if that means discarding old data. If an
        // MGS instance is actually attached, we should be flushing packets out
        // to it fast enough that we don't see this in practice.
        match self.client {
            UartClient::Humility => {
                while !self.from_rx.is_full() {
                    let Some(b) = self.usart.try_rx_pop() else {
                        break;
                    };
                    self.from_rx.push_back(b).unwrap_lite();
                    n_received += 1;
                }

                if self.from_rx.is_full() {
                    // If our buffer is full, we need to disable the RX
                    // interrupt until Humility makes space (or detaches). We'll
                    // re-enable the rx interrupt in either:
                    //
                    // 1. `uart_read()` when Humility polls us for data
                    // 2. `set_client()` if our client is set back to Mgs
                    self.usart.disable_rx_interrupt();
                }
            }
            UartClient::Mgs => {
                while let Some(b) = self.usart.try_rx_pop() {
                    n_received += 1;
                    match self.from_rx.push_back(b) {
                        Ok(()) => (),
                        Err(b) => {
                            // If `push_back` failed, we know there is at least
                            // one item, allowing us to unwrap `pop_front`. At
                            // that point we know there's space for at least
                            // one, allowing us to unwrap a subsequent
                            // `push_back`.
                            self.from_rx.pop_front().unwrap_lite();
                            self.from_rx.push_back(b).unwrap_lite();
                            discarded_data += 1;
                        }
                    }
                }
            }
        }

        // Update our offset, which will allow MGS to know we discarded data,
        // and log that fact locally via ringbuf.
        self.from_rx_offset += discarded_data;
        if discarded_data > 0 {
            ringbuf_entry_root!(Log::UsartRxBufferDataDropped {
                num_bytes: discarded_data
            });
        }

        if n_received > 0 && self.from_rx_flush_deadline.is_none() {
            self.set_from_rx_flush_deadline();
        }

        // Re-enable USART interrupts.
        sys_irq_control(notifications::USART_IRQ_MASK, true);
    }

    fn send_break(&self) {
        self.usart.send_break();
    }
}

fn configure_usart() -> Usart {
    use drv_stm32h7_usart::device;
    use drv_stm32h7_usart::drv_stm32xx_sys_api::*;

    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 100_000_000;

    // For gimlet, we only expect baud rate 3 Mbit, usart1, with hardware flow
    // control enabled. We could expand our cargo features to cover other cases
    // as needed. Currently, failing to enable any of those three features will
    // cause a compilation error.
    #[cfg(feature = "baud_rate_3M")]
    const BAUD_RATE: u32 = 3_000_000;

    #[cfg(all(feature = "usart1", feature = "usart1-gimletlet"))]
    compile_error!(concat!(
        "at most one usart feature (`usart1`, `usart1-gimletlet`)",
        " should be enabled",
    ));

    cfg_if::cfg_if! {
        if #[cfg(feature = "usart1")] {
            const PINS: &[(PinSet, Alternate)] = &[(
                Port::A.pin(9).and_pin(10).and_pin(11).and_pin(12),
                Alternate::AF7
            )];

            // From thin air, pluck a pointer to the USART register block.
            //
            // Safety: this is needlessly unsafe in the API. The USART is
            // essentially a static, and we access it through a & reference so
            // aliasing is not a concern. Were it literally a static, we could
            // just reference it.
            let usart = unsafe { &*device::USART1::ptr() };
            let peripheral = Peripheral::Usart1;
            let pins = PINS;
        } else if #[cfg(feature = "usart1-gimletlet")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::A.pin(11).and_pin(12), Alternate::AF7),
                (Port::B.pin(6).and_pin(7), Alternate::AF7),
            ];

            // From thin air, pluck a pointer to the USART register block.
            //
            // Safety: this is needlessly unsafe in the API. The USART is
            // essentially a static, and we access it through a & reference so
            // aliasing is not a concern. Were it literally a static, we could
            // just reference it.
            let usart = unsafe { &*device::USART1::ptr() };
            let peripheral = Peripheral::Usart1;
            let pins = PINS;
        } else {
            compile_error!("no usartX feature specified");
        }
    }

    Usart::turn_on(
        &Sys::from(SYS.get_task_id()),
        usart,
        peripheral,
        pins,
        CLOCK_HZ,
        BAUD_RATE,
        true, // hardware_flow_control
    )
}

trait DequeExt {
    fn drain_front(&mut self, n: usize);
}

impl<T, const N: usize> DequeExt for Deque<T, N> {
    fn drain_front(&mut self, n: usize) {
        for _ in 0..n {
            self.pop_front().unwrap_lite();
        }
    }
}
