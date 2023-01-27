// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, notifications, update::host_flash::HostFlashUpdate,
    update::rot::RotUpdate, update::sp::SpUpdate, update::ComponentUpdater,
    usize_max, vlan_id_from_sp_port, Log, MgsMessage, SYS,
};
use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, Ordering};
use drv_gimlet_seq_api::Sequencer;
use drv_stm32h7_usart::Usart;
use gateway_messages::sp_impl::{
    BoundsChecked, DeviceDescription, SocketAddrV6, SpHandler,
};
use gateway_messages::{
    ignition, ComponentDetails, ComponentUpdatePrepare, DiscoverResponse,
    Header, IgnitionCommand, IgnitionState, Message, MessageKind, MgsError,
    PowerState, SpComponent, SpError, SpPort, SpRequest, SpState,
    SpUpdatePrepare, UpdateChunk, UpdateId, UpdateStatus,
};
use heapless::Deque;
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_control_plane_agent_api::{
    ControlPlaneAgentError, UartClient, VpdIdentity,
};
use task_net_api::{Address, MacAddress, UdpMetadata};
use userlib::{sys_get_timer, sys_irq_control, UnwrapLite};

// We're included under a special `path` cfg from main.rs, which confuses rustc
// about where our submodules live. Pass explicit paths to correct it.
#[path = "mgs_gimlet/host_phase2.rs"]
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
userlib::task_slot!(GIMLET_SEQ, gimlet_seq);

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sequencer: Sequencer,
    sp_update: SpUpdate,
    rot_update: RotUpdate,
    host_flash_update: HostFlashUpdate,
    host_phase2: HostPhase2Requester,
    usart: UsartHandler,
    startup_options: HostStartupOptions,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
    serial_console_write_offset: u64,
    next_message_id: u32,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        let usart = UsartHandler::claim_static_resources();

        // XXX For now, we want to default to these options.
        let startup_options = HostStartupOptions::STARTUP_KMDB
            | HostStartupOptions::STARTUP_PROM
            | HostStartupOptions::STARTUP_VERBOSE;

        Self {
            common: MgsCommon::claim_static_resources(base_mac_address),
            host_flash_update: HostFlashUpdate::new(),
            host_phase2: HostPhase2Requester::claim_static_resources(),
            sp_update: SpUpdate::new(),
            rot_update: RotUpdate::new(),
            sequencer: Sequencer::from(GIMLET_SEQ.get_task_id()),
            usart,
            startup_options,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
            next_message_id: 0,
        }
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        self.common.identity()
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
            || self.sp_update.is_preparing()
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
        self.sp_update.step_preparation();
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

        // Do we have an attached MGS instance?
        let (mgs_addr, sp_port) = match self.attached_serial_console_mgs {
            Some((mgs_addr, sp_port)) => (mgs_addr, sp_port),
            None => {
                // Discard any buffered data and reset any usart-related timers.
                self.usart.clear_rx_data();
                return None;
            }
        };

        // We have data we want to flush and an attached MGS; build our packet.
        ringbuf_entry!(Log::SerialConsoleSend {
            buffered: self.usart.from_rx.len(),
        });

        let message = Message {
            header: Header {
                version: gateway_messages::version::V2,
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
            addr: Address::Ipv6(mgs_addr.ip.into()),
            port: mgs_addr.port,
            size: n as u32,
            vid: vlan_id_from_sp_port(sp_port),
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

    pub(crate) fn startup_options(
        &self,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        Ok(self.startup_options)
    }

    pub(crate) fn set_startup_options(
        &mut self,
        startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        self.startup_options = startup_options;
        Ok(())
    }

    fn power_state_impl(&self) -> Result<PowerState, SpError> {
        use drv_gimlet_seq_api::PowerState as DrvPowerState;

        // TODO Do we want to expose the sub-states to the control plane? For
        // now, squish them down.
        //
        // TODO Do we want to expose A1 to the control plane at all? If not,
        // what would we map it to? Maybe easier to leave it exposed.
        let state = match self
            .sequencer
            .get_state()
            .map_err(|e| SpError::PowerStateError(e as u32))?
        {
            DrvPowerState::A2
            | DrvPowerState::A2PlusMono
            | DrvPowerState::A2PlusFans => PowerState::A2,
            DrvPowerState::A1 => PowerState::A1,
            DrvPowerState::A0
            | DrvPowerState::A0PlusHP
            | DrvPowerState::A0Thermtrip => PowerState::A0,
        };

        Ok(state)
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
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.prepare(&UPDATE_MEMORY, update)
            }
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.rot_update.prepare(&UPDATE_MEMORY, update)
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
            SpComponent::HOST_CPU_BOOT_FLASH => self
                .host_flash_update
                .ingest_chunk(&chunk.id, chunk.offset, data),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.rot_update.ingest_chunk(&chunk.id, chunk.offset, data)
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

        let status = match component {
            // Unlike `update_chunk()`, we only need to match on `SP_ITSELF`
            // here and not `SP_AUX_FLASH`. We mostly hide the fact that an SP
            // update also may include an aux flash image from clients: when
            // they start an update, it is for `SP_ITSELF` (whether or not it
            // includes an aux flash image, which they don't need to know).
            // Similarly, they will only ask for the status of an `SP_ITSELF`
            // update, not an `SP_AUX_FLASH` update (which isn't a thing).
            SpComponent::SP_ITSELF => self.sp_update.status(),
            SpComponent::HOST_CPU_BOOT_FLASH => self.host_flash_update.status(),
            SpComponent::ROT | SpComponent::STAGE0 => self.rot_update.status(),
            _ => return Err(SpError::RequestUnsupportedForComponent),
        };

        Ok(status)
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
            // Unlike `update_chunk()`, we only need to match on `SP_ITSELF`
            // here and not `SP_AUX_FLASH`. We mostly hide the fact that an SP
            // update also may include an aux flash image from clients: when
            // they start an update, it is for `SP_ITSELF` (whether or not it
            // includes an aux flash image, which they don't need to know).
            // Similarly, they will only attempt to abort an `SP_ITSELF`
            // update, not an `SP_AUX_FLASH` update (which isn't a thing).
            SpComponent::SP_ITSELF => self.sp_update.abort(&id),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.abort(&id)
            }
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
        use drv_gimlet_seq_api::PowerState as DrvPowerState;
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SetPowerState(power_state)));

        let power_state = match power_state {
            PowerState::A0 => DrvPowerState::A0,
            PowerState::A1 => DrvPowerState::A1,
            PowerState::A2 => DrvPowerState::A2,
        };

        self.sequencer
            .set_state(power_state)
            .map_err(|e| SpError::PowerStateError(e as u32))
    }

    fn serial_console_attach(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
        component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));

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
        self.attached_serial_console_mgs = Some((sender, port));
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
        sender: SocketAddrV6,
        port: SpPort,
        mut offset: u64,
        mut data: &[u8],
    ) -> Result<u64, SpError> {
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
            return Err(SpError::SerialConsoleNotAttached);
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
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        self.attached_serial_console_mgs = None;
        Ok(())
    }

    fn serial_console_break(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SerialConsoleBreak));
        // TODO: same caveats as above!
        if Some((sender, port)) != self.attached_serial_console_mgs {
            return Err(SpError::SerialConsoleNotAttached);
        }
        self.usart.send_break();
        Ok(())
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

    fn get_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::GetStartupOptions));

        Ok(self.startup_options.into())
    }

    fn set_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SetStartupOptions(options)));

        self.startup_options = options.into();

        Ok(())
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

        match component {
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.active_slot()
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_set_active_slot(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
        slot: u16,
    ) -> Result<(), SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ComponentSetActiveSlot {
            component,
            slot,
        }));
        match component {
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.set_active_slot(slot)
            }
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
        port: SpPort,
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

        self.host_phase2
            .ingest_incoming_data(port, hash, offset, data);
    }

    fn send_host_nmi(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), SpError> {
        // This can only fail if the `gimlet-seq` server is dead; in that
        // case, send `Busy` because it should be rebooting.
        ringbuf_entry!(Log::MgsMessage(MgsMessage::SendHostNmi));
        self.sequencer
            .send_hardware_nmi()
            .map_err(|_| SpError::Busy)?;
        Ok(())
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
    fn claim_static_resources() -> Self {
        let usart = configure_usart();
        let to_tx = claim_mgs_to_sp_usart_buf_static();
        let from_rx = claim_sp_to_mgs_usart_buf_static();

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
            ringbuf_entry!(Log::UsartTxFull {
                remaining: self.to_tx.len()
            });
        }

        // Clear any errors.
        if self.usart.check_and_clear_rx_overrun() {
            ringbuf_entry!(Log::UsartRxOverrun);
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
            ringbuf_entry!(Log::UsartRxBufferDataDropped {
                num_bytes: discarded_data
            });
        }

        if n_received > 0 {
            if self.from_rx_flush_deadline.is_none() {
                self.set_from_rx_flush_deadline();
            }
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

fn claim_mgs_to_sp_usart_buf_static(
) -> &'static mut Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE> {
    static mut UART_TX_BUF: Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE> =
        Deque::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `UART_TX_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut UART_TX_BUF }
}

fn claim_sp_to_mgs_usart_buf_static(
) -> &'static mut Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE> {
    static mut UART_RX_BUF: Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE> =
        Deque::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `UART_RX_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut UART_RX_BUF }
}
