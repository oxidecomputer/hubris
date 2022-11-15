// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, update::host_flash::HostFlashUpdate,
    update::sp::SpUpdate, update::ComponentUpdater, usize_max,
    vlan_id_from_sp_port, Log, MgsMessage, SYS, USART_IRQ,
};
use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, Ordering};
use drv_gimlet_seq_api::Sequencer;
use drv_stm32h7_usart::Usart;
use gateway_messages::sp_impl::{DeviceDescription, SocketAddrV6, SpHandler};
use gateway_messages::{
    BulkIgnitionState, ComponentDetails, ComponentUpdatePrepare,
    DiscoverResponse, Header, IgnitionCommand, IgnitionState, Message,
    MessageKind, MgsError, PowerState, SpComponent, SpError, SpPort, SpRequest,
    SpState, SpUpdatePrepare, UpdateChunk, UpdateId, UpdateStatus,
};
use heapless::Deque;
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::ringbuf_entry_root;
use task_control_plane_agent_api::ControlPlaneAgentError;
use task_net_api::{Address, UdpMetadata};
use userlib::{sys_get_timer, sys_irq_control, UnwrapLite};

// We're included under a special `path` cfg from main.rs, which confuses rustc
// about where our submodules live. Pass explicit paths to correct it.
#[path = "mgs_gimlet/host_phase2.rs"]
mod host_phase2;

use host_phase2::HostPhase2Requester;

// How big does our shared update buffer need to be? Has to be able to handle SP
// update blocks or host flash pages.
const UPDATE_BUFFER_SIZE: usize =
    usize_max(SpUpdate::BLOCK_SIZE, HostFlashUpdate::BLOCK_SIZE);

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
/// we are to lose data while waiting to flush from our buffer out to UDP.
const MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE: usize =
    gateway_messages::MAX_SERIALIZED_SIZE;
const SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE: usize =
    gateway_messages::MAX_SERIALIZED_SIZE;

/// Send any buffered serial console data to MGS when our oldest buffered byte
/// is this old, even if our buffer isn't full yet.
const SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS: u64 = 500;

userlib::task_slot!(HOST_FLASH, hf);
userlib::task_slot!(GIMLET_SEQ, gimlet_seq);

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sequencer: Sequencer,
    sp_update: SpUpdate,
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
    pub(crate) fn claim_static_resources() -> Self {
        let usart = UsartHandler::claim_static_resources();

        // XXX For now, we want to default to these options.
        let startup_options =
            HostStartupOptions::DEBUG_KMDB | HostStartupOptions::DEBUG_PROM;

        Self {
            common: MgsCommon::claim_static_resources(),
            host_flash_update: HostFlashUpdate::new(),
            host_phase2: HostPhase2Requester::claim_static_resources(),
            sp_update: SpUpdate::new(),
            sequencer: Sequencer::from(GIMLET_SEQ.get_task_id()),
            usart,
            startup_options,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
            next_message_id: 0,
        }
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

    pub(crate) fn drive_usart(&mut self) {
        self.usart.run_until_blocked();
    }

    pub(crate) fn wants_to_send_packet_to_mgs(&mut self) -> bool {
        // Do we have an attached serial console session MGS?
        if self.attached_serial_console_mgs.is_none() {
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
        ringbuf_entry_root!(Log::SerialConsoleSend {
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
}

impl SpHandler for MgsHandler {
    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
        self.common.discover(port)
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionState {
            target
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<BulkIgnitionState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::BulkIgnitionState));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionCommand {
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
        self.common.sp_state()
    }

    fn sp_update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: SpUpdatePrepare,
    ) -> Result<(), SpError> {
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
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
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
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_status(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
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
            SpComponent::SP_ITSELF => self.sp_update.status(),
            SpComponent::HOST_CPU_BOOT_FLASH => self.host_flash_update.status(),
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
            SpComponent::SP_ITSELF => self.sp_update.abort(&id),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.abort(&id)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<PowerState, SpError> {
        use drv_gimlet_seq_api::PowerState as DrvPowerState;
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetPowerState));

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

    fn set_power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        power_state: PowerState,
    ) -> Result<(), SpError> {
        use drv_gimlet_seq_api::PowerState as DrvPowerState;
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetPowerState(
            power_state
        )));

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
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        self.attached_serial_console_mgs = None;
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
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::Inventory));
        self.common.inventory_num_devices() as u32
    }

    fn device_description(&mut self, index: u32) -> DeviceDescription<'_> {
        self.common.inventory_device_description(index as usize)
    }

    fn get_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetStartupOptions));

        Ok(self.startup_options.into())
    }

    fn set_startup_options(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetStartupOptions(
            options
        )));

        self.startup_options = options.into();

        Ok(())
    }

    fn num_component_details(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<u32, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::ComponentDetails {
            component
        }));

        // TODO: Wire up any component info we can (sensor measurements, etc)
        match component {
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_details(
        &mut self,
        _component: SpComponent,
        _index: u32,
    ) -> ComponentDetails {
        // We never return successfully from `num_component_details()`, so this
        // function should never be called.
        panic!()
    }

    fn mgs_response_error(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        message_id: u32,
        err: MgsError,
    ) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::MgsError {
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
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::HostPhase2Data {
            hash,
            offset,
            data_len: data.len(),
        }));

        self.host_phase2
            .ingest_incoming_data(port, hash, offset, data);
    }
}

struct UsartHandler {
    usart: Usart,
    to_tx: &'static mut Deque<u8, MGS_TO_SP_SERIAL_CONSOLE_BUFFER_SIZE>,
    from_rx: &'static mut Deque<u8, SP_TO_MGS_SERIAL_CONSOLE_BUFFER_SIZE>,
    from_rx_flush_deadline: Option<u64>,
    from_rx_offset: u64,
}

impl UsartHandler {
    fn claim_static_resources() -> Self {
        let usart = configure_usart();
        let to_tx = claim_mgs_to_sp_usart_buf_static();
        let from_rx = claim_sp_to_mgs_usart_buf_static();

        // Enbale USART interrupts.
        sys_irq_control(USART_IRQ, true);

        Self {
            usart,
            to_tx,
            from_rx,
            from_rx_flush_deadline: None,
            from_rx_offset: 0,
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

    fn should_flush_to_mgs(&self) -> bool {
        // Bail out early if our buffer is empty or full.
        let len = self.from_rx.len();
        if len == 0 {
            return false;
        } else if len == self.from_rx.capacity() {
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

        // Clean up / ringbuf debug log after transmitting.
        if n_transmitted > 0 {
            ringbuf_entry_root!(Log::UsartTx {
                num_bytes: n_transmitted
            });
            self.to_tx.drain_front(n_transmitted);
        }
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

        // Recv as much as we can from the USART FIFO, even if we have to
        // discard old data to do so.
        let mut n_received = 0;
        let mut discarded_data = 0;
        while let Some(b) = self.usart.try_rx_pop() {
            n_received += 1;
            match self.from_rx.push_back(b) {
                Ok(()) => (),
                Err(b) => {
                    // If `push_back` failed, we know there is at least one
                    // item, allowing us to unwrap `pop_front`. At that point we
                    // know there's space for at least one, allowing us to
                    // unwrap a subsequent `push_back`.
                    self.from_rx.pop_front().unwrap_lite();
                    self.from_rx.push_back(b).unwrap_lite();
                    discarded_data += 1;
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

        if n_received > 0 {
            ringbuf_entry_root!(Log::UsartRx {
                num_bytes: n_received
            });
            if self.from_rx_flush_deadline.is_none() {
                self.set_from_rx_flush_deadline();
            }
        }

        // Re-enable USART interrupts.
        sys_irq_control(USART_IRQ, true);
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

    #[cfg(feature = "hardware_flow_control")]
    let hardware_flow_control = true;

    cfg_if::cfg_if! {
        if #[cfg(feature = "usart1")] {
            const PINS: &[(PinSet, Alternate)] = {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "hardware_flow_control")] {
                        &[(
                            Port::A.pin(9).and_pin(10).and_pin(11).and_pin(12),
                            Alternate::AF7
                        )]
                    } else {
                        compile_error!("hardware_flow_control feature must be enabled");
                    }
                }
            };

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
        hardware_flow_control,
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
