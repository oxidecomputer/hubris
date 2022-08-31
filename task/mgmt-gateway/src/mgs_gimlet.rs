// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, vlan_id_from_sp_port, Log, MgsMessage, SYS,
    TIMER_IRQ, USART_IRQ, __RINGBUF,
};
use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, Ordering};
use drv_stm32h7_usart::Usart;
use gateway_messages::{
    sp_impl::SocketAddrV6, sp_impl::SpHandler, BulkIgnitionState,
    DiscoverResponse, IgnitionCommand, IgnitionState, ResponseError,
    SpComponent, SpMessage, SpMessageKind, SpPort, SpState, UpdateChunk,
    UpdateStart,
};
use heapless::Deque;
use ringbuf::ringbuf_entry;
use task_net_api::{Address, UdpMetadata};
use userlib::{sys_get_timer, sys_irq_control, sys_set_timer, UnwrapLite};

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

pub(crate) struct MgsHandler {
    common: MgsCommon,
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
            usart,
            attached_serial_console_mgs: None,
            serial_console_write_offset: 0,
        }
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
        ringbuf_entry!(Log::SerialConsoleSend {
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
        self.common.sp_state()
    }

    fn update_start(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: UpdateStart,
    ) -> Result<(), ResponseError> {
        self.common.update_start(update)
    }

    fn update_chunk(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        self.common.update_chunk(chunk, data)
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
        sys_set_timer(None, TIMER_IRQ);
    }

    fn drain_flushed_data(&mut self, n: usize) {
        self.from_rx.drain_front(n);
        self.from_rx_offset += n as u64;
        self.from_rx_flush_deadline = None;
        self.start_flush_timer_if_needed();
    }

    fn start_flush_timer_if_needed(&mut self) {
        if self.from_rx_flush_deadline.is_none() && !self.from_rx.is_empty() {
            let deadline =
                sys_get_timer().now + SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS;
            self.from_rx_flush_deadline = Some(deadline);
            sys_set_timer(Some(deadline), TIMER_IRQ);
        }
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
            ringbuf_entry!(Log::UsartTx {
                num_bytes: n_transmitted
            });
            self.to_tx.drain_front(n_transmitted);
        }
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
            ringbuf_entry!(Log::UsartRxBufferDataDropped {
                num_bytes: discarded_data
            });
        }

        if n_received > 0 {
            ringbuf_entry!(Log::UsartRx {
                num_bytes: n_received
            });
            self.start_flush_timer_if_needed();
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
