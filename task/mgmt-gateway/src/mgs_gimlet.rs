// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, update_buffer::UpdateBuffer, vlan_id_from_sp_port,
    Log, MgsMessage, SYS, USART_IRQ,
};
use core::convert::Infallible;
use core::ops::Range;
use core::sync::atomic::{AtomicBool, Ordering};
use drv_gimlet_hf_api::{
    HfDevSelect, HfError, HfMuxState, HostFlash, PAGE_SIZE_BYTES,
    SECTOR_SIZE_BYTES,
};
use drv_stm32h7_usart::Usart;
use gateway_messages::{
    sp_impl::SocketAddrV6, sp_impl::SpHandler, BulkIgnitionState,
    DiscoverResponse, IgnitionCommand, IgnitionState, ResponseError,
    SpComponent, SpMessage, SpMessageKind, SpPort, SpState, UpdateChunk,
    UpdateId, UpdatePreparationProgress, UpdatePreparationStatus,
    UpdatePrepare, UpdateStatus,
};
use heapless::Deque;
use ringbuf::ringbuf_entry_root;
use task_net_api::{Address, UdpMetadata};
use userlib::{sys_get_timer, sys_irq_control, UnwrapLite};

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

pub(crate) struct MgsHandler {
    common: MgsCommon,
    host_flash_update: HostFlashUpdate,
    usart: UsartHandler,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
    serial_console_write_offset: u64,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources() -> Self {
        let usart = UsartHandler::claim_static_resources();
        Self {
            common: MgsCommon::claim_static_resources(),
            host_flash_update: HostFlashUpdate::claim_static_resources(),
            usart,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
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
        if self.host_flash_update.needs_sectors_erased() {
            Some(sys_get_timer().now + 1)
        } else {
            self.usart.from_rx_flush_deadline
        }
    }

    pub(crate) fn handle_timer_fired(&mut self) {
        self.host_flash_update.erase_sectors_if_needed();
        // Even though `timer_deadline()` can return a timer related to usart
        // flushing, we don't need to do anything here; `NetHandler` in main.rs
        // will call `wants_to_send_packet_to_mgs()` below when it's ready to
        // grab any data we want to flush.
    }

    pub(crate) fn drive_usart(&mut self) {
        self.usart.run_until_blocked();
    }

    pub(crate) fn wants_to_send_packet_to_mgs(&mut self) -> bool {
        // Do we have an attached serial console session MGS?
        if self.attached_serial_console_mgs.is_none() {
            self.usart.clear_rx_data();
            return false;
        }

        self.usart.should_flush_to_mgs()
    }

    pub(crate) fn packet_to_mgs(
        &mut self,
        tx_buf: &mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    ) -> Option<UdpMetadata> {
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

        let message = SpMessage {
            version: gateway_messages::version::V1,
            kind: SpMessageKind::SerialConsole {
                component: SpComponent::SP3_HOST_CPU,
                offset: self.usart.from_rx_offset,
            },
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

    fn update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: UpdatePrepare,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        match update.component {
            SpComponent::SP_ITSELF => self.common.update_prepare(update),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.prepare(update)
            }
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
            SpComponent::SP_ITSELF => self.common.update_chunk(chunk, data),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.ingest_chunk(chunk, data)
            }
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
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
            SpComponent::SP_ITSELF => Ok(self.common.status()),
            SpComponent::HOST_CPU_BOOT_FLASH => self.host_flash_update.status(),
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
            SpComponent::SP_ITSELF => self.common.update_abort(&id),
            SpComponent::HOST_CPU_BOOT_FLASH => {
                self.host_flash_update.abort(&id)
            }
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn serial_console_attach(
        &mut self,
        sender: SocketAddrV6,
        port: SpPort,
        component: SpComponent,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));

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
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        self.attached_serial_console_mgs = None;
        Ok(())
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

    const BAUD_RATE: u32 = 115_200;

    let usart;
    let peripheral;
    let pins;

    cfg_if::cfg_if! {
        if #[cfg(feature = "usart1")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::B.pin(6).and_pin(7), Alternate::AF7),
            ];

            // From thin air, pluck a pointer to the USART register block.
            //
            // Safety: this is needlessly unsafe in the API. The USART is
            // essentially a static, and we access it through a & reference so
            // aliasing is not a concern. Were it literally a static, we could
            // just reference it.
            usart = unsafe { &*device::USART1::ptr() };
            peripheral = Peripheral::Usart1;
            pins = PINS;
        } else if #[cfg(feature = "usart2")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::D.pin(5).and_pin(6), Alternate::AF7),
            ];
            usart = unsafe { &*device::USART2::ptr() };
            peripheral = Peripheral::Usart2;
            pins = PINS;
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

struct HostFlashUpdate {
    task: HostFlash,
    buf: UpdateBuffer<HostFlash, PAGE_SIZE_BYTES>,
    sector_erase: Option<HostFlashSectorErase>,
}

impl HostFlashUpdate {
    fn claim_static_resources() -> Self {
        let buf = claim_hf_update_buffer_static();
        Self {
            task: HostFlash::from(HOST_FLASH.get_task_id()),
            buf: UpdateBuffer::new(
                buf,
                |hf_task, block_index, data| {
                    let address = (block_index * PAGE_SIZE_BYTES) as u32;
                    hf_task
                        .page_program(address, data)
                        .map_err(|err| ResponseError::UpdateFailed(err as u32))
                },
                |_hf_task| {
                    // nothing to do to finalize?
                    // TODO should we set_dev() back to what it was (if we
                    // changed it)?
                    Ok(())
                },
            ),
            sector_erase: None,
        }
    }

    fn status(&self) -> Result<UpdateStatus, ResponseError> {
        let status = self.buf.status();
        match &status {
            // `UpdateBuffer` never sets its status to "preparing", as it
            // doesn't know anything about that stage.
            UpdateStatus::Preparing { .. } => panic!(),
            UpdateStatus::InProgress(sub_status) => {
                if let Some(sector_erase) = self.sector_erase.as_ref() {
                    // If we're still erasing sectors, we shouldn't have
                    // ingested any chunks yet.
                    assert!(sub_status.bytes_received == 0);

                    let progress = sector_erase.progress()?;
                    Ok(UpdateStatus::Preparing(UpdatePreparationStatus {
                        id: sub_status.id,
                        progress: Some(progress),
                    }))
                } else {
                    Ok(status)
                }
            }
            UpdateStatus::None
            | UpdateStatus::Complete(_)
            | UpdateStatus::Aborted(_) => Ok(status),
        }
    }

    fn needs_sectors_erased(&self) -> bool {
        self.sector_erase.is_some()
    }

    fn erase_sectors_if_needed(&mut self) {
        if let Some(sector_erase) = self.sector_erase.as_mut() {
            sector_erase.erase_sectors_if_needed(&self.task);
            if sector_erase.is_done() {
                self.sector_erase = None;
            }
        }
    }

    fn prepare(&mut self, update: UpdatePrepare) -> Result<(), ResponseError> {
        // Which slot are we updating?
        let slot = match update.slot {
            0 => HfDevSelect::Flash0,
            1 => HfDevSelect::Flash1,
            _ => return Err(ResponseError::InvalidSlotForComponent),
        };

        // Do we have control of the host flash?
        match self
            .task
            .get_mux()
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?
        {
            HfMuxState::SP => (),
            HfMuxState::HostCPU => return Err(ResponseError::UpdateSlotBusy),
        }

        // Is an update already in progress?
        self.buf.ensure_no_update_in_progress()?;

        // Swap to the chosen slot.
        self.task
            .set_dev(slot)
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        // What is the total capacity of the device?
        let capacity = self
            .task
            .capacity()
            .map_err(|err| ResponseError::UpdateFailed(err as u32))?;

        // How many total sectors do we need to erase? For gimlet, we know that
        // capacity is an exact multiple of the sector size, which is probably
        // a safe assumption for future parts as well. We'll assert here in case
        // that ever becomes untrue, and we can update our math.
        assert!(capacity % SECTOR_SIZE_BYTES == 0);
        self.sector_erase =
            Some(HostFlashSectorErase::new(capacity / SECTOR_SIZE_BYTES));

        self.buf.start(update.id, update.total_size);

        Ok(())
    }

    fn ingest_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        // Have we finished erasing the host flash?
        if self.needs_sectors_erased() {
            return Err(ResponseError::UpdateNotPrepared);
        }

        self.buf
            .ingest_chunk(&chunk.id, &self.task, chunk.offset, data)
    }

    fn abort(&mut self, id: &UpdateId) -> Result<(), ResponseError> {
        // We will allow the abort if either:
        //
        // 1. We have an in-progress update that matches `id`
        // 2. We do not have an in-progress update
        //
        // We only want to return an error if we have a _different_ in-progress
        // update.
        if let Some(in_progress_id) = self.buf.in_progress_update_id() {
            if id != in_progress_id {
                return Err(ResponseError::UpdateInProgress(self.buf.status()));
            }
        }

        // TODO should we erase the slot?
        // TODO should we set_dev() back to what it was (if we changed it)?
        self.buf.abort();
        self.sector_erase = None;
        Ok(())
    }
}

struct HostFlashSectorErase {
    sectors_to_erase: Range<usize>,
    most_recent_error: Option<HfError>,
}

impl HostFlashSectorErase {
    fn new(num_sectors: usize) -> Self {
        Self {
            sectors_to_erase: 0..num_sectors,
            most_recent_error: None,
        }
    }

    fn is_done(&self) -> bool {
        self.sectors_to_erase.is_empty()
    }

    fn progress(&self) -> Result<UpdatePreparationProgress, ResponseError> {
        if let Some(err) = self.most_recent_error {
            Err(ResponseError::UpdateFailed(err as u32))
        } else {
            Ok(UpdatePreparationProgress {
                current: self.sectors_to_erase.start as u32,
                total: self.sectors_to_erase.end as u32,
            })
        }
    }

    fn erase_sectors_if_needed(&mut self, task: &HostFlash) {
        // While we're erasing sectors, we're not able to service other
        // interrupts (e.g., incoming requests from MGS). We therefore limit how
        // many sectors we're willing to erase in one call to this function, and
        // it's our callers responsibility to continue to call us until we're
        // done.
        //
        // Empirically, erasing 8 sectors can take up to a second, and raising
        // it higher does not significantly improve our throughput.
        const MAX_SECTORS_TO_ERASE_ONE_CALL: usize = 8;

        if self.is_done() {
            return;
        }

        let num_sectors = usize::min(
            MAX_SECTORS_TO_ERASE_ONE_CALL,
            self.sectors_to_erase.end - self.sectors_to_erase.start,
        );
        for i in 0..num_sectors {
            let sector = self.sectors_to_erase.start + i;
            let addr = sector * SECTOR_SIZE_BYTES;
            match task.sector_erase(addr as u32) {
                Ok(()) => (),
                Err(err) => {
                    self.sectors_to_erase.start += i;
                    self.most_recent_error = Some(err);
                    return;
                }
            }
        }

        self.sectors_to_erase.start += num_sectors;
        self.most_recent_error = None;
        ringbuf_entry_root!(Log::HostFlashSectorsErased { num_sectors });
    }
}

fn claim_hf_update_buffer_static(
) -> &'static mut heapless::Vec<u8, PAGE_SIZE_BYTES> {
    static mut HF_UPDATE_BUF: heapless::Vec<u8, PAGE_SIZE_BYTES> =
        heapless::Vec::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `HF_UPDATE_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut HF_UPDATE_BUF }
}
