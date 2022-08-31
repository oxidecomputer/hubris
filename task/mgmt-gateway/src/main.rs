// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_stm32h7_usart::Usart;
use gateway_messages::{
    sp_impl, sp_impl::Error as MgsDispatchError, IgnitionCommand, SpComponent,
    SpMessage, SpMessageKind, SpPort,
};
use heapless::Deque;
use mgs_handler::UsartFlush;
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use task_net_api::{
    Address, LargePayloadBehavior, Net, RecvError, SendError, SocketName,
    UdpMetadata,
};
use userlib::{
    sys_get_timer, sys_irq_control, sys_recv_closed, sys_set_timer, task_slot,
    TaskId, UnwrapLite,
};

mod mgs_handler;

use self::mgs_handler::MgsHandler;

type SerializedMessageBuf = [u8; gateway_messages::MAX_SERIALIZED_SIZE];

task_slot!(JEFE, jefe);
task_slot!(NET, net);
task_slot!(SYS, sys);
task_slot!(UPDATE_SERVER, update_server);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Log {
    Empty,
    Wake(u32),
    Rx(UdpMetadata),
    DispatchError(MgsDispatchError),
    SendError(SendError),
    MgsMessage(MgsMessage),
    UsartTx { num_bytes: usize },
    UsartTxFull { remaining: usize },
    UsartRx { num_bytes: usize },
    UsartRxOverrun,
    UsartRxBufferDataDropped { num_bytes: u64 },
    SerialConsoleSend { buffered: usize },
    UpdatePartial { bytes_written: usize },
    UpdateComplete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MgsMessage {
    Discovery,
    IgnitionState {
        target: u8,
    },
    BulkIgnitionState,
    IgnitionCommand {
        target: u8,
        command: IgnitionCommand,
    },
    SpState,
    SerialConsoleAttach,
    SerialConsoleWrite {
        offset: u64,
        length: u16,
    },
    SerialConsoleDetach,
    UpdateStart {
        length: u32,
    },
    UpdateChunk {
        offset: u32,
    },
    SysResetPrepare,
}

ringbuf!(Log, 16, Log::Empty);

// Must match app.toml!
const NET_IRQ: u32 = 1 << 0;
const USART_IRQ: u32 = 1 << 1;

// Must not conflict with IRQs above!
const TIMER_IRQ: u32 = 1 << 2;

// Send any buffered serial console data to MGS when our oldest buffered byte is
// this old, even if our buffer isn't full yet.
const SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS: u64 = 500;

const SOCKET: SocketName = SocketName::mgmt_gateway;

#[export_name = "main"]
fn main() {
    let usart = UsartHandler::new(configure_usart(), claim_uart_bufs_static());
    let mut mgs_handler = MgsHandler::new(usart);
    let mut net_handler = NetHandler::new(claim_net_bufs_static());

    // Enbale USART interrupts.
    sys_irq_control(USART_IRQ, true);

    loop {
        let note = sys_recv_closed(
            &mut [],
            NET_IRQ | USART_IRQ | TIMER_IRQ,
            TaskId::KERNEL,
        )
        .unwrap_lite()
        .operation;
        ringbuf_entry!(Log::Wake(note));

        if (note & USART_IRQ) != 0 {
            mgs_handler.usart.run_until_blocked();
            sys_irq_control(USART_IRQ, true);
        }

        if (note & NET_IRQ) != 0 || mgs_handler.needs_usart_flush_to_mgs() {
            net_handler.run_until_blocked(&mut mgs_handler);
        }
    }
}

struct UsartHandler {
    usart: Usart,
    to_tx: &'static mut Deque<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>,
    from_rx: &'static mut Deque<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>,
    from_rx_flush_deadline: Option<u64>,
    from_rx_offset: u64,
}

impl UsartHandler {
    fn new(
        usart: Usart,
        buffers: [&'static mut Deque<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>;
            2],
    ) -> Self {
        let [to_tx, from_rx] = buffers;
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
    }
}

struct NetHandler {
    net: Net,
    tx_buf: &'static mut SerializedMessageBuf,
    rx_buf: &'static mut SerializedMessageBuf,
    packet_to_send: Option<UdpMetadata>,
}

impl NetHandler {
    fn new(buffers: &'static mut [SerializedMessageBuf; 2]) -> Self {
        let [tx_buf, rx_buf] = buffers;
        Self {
            net: Net::from(NET.get_task_id()),
            tx_buf,
            rx_buf,
            packet_to_send: None,
        }
    }

    fn run_until_blocked(&mut self, mgs_handler: &mut MgsHandler) {
        loop {
            // Try to send first.
            if let Some(meta) = self.packet_to_send.take() {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_buf[..meta.size as usize],
                ) {
                    Ok(()) => (),
                    Err(err @ SendError::QueueFull) => {
                        ringbuf_entry!(Log::SendError(err));

                        // "Re-enqueue" packet and return; we'll wait until
                        // `net` wakes us again to retry.
                        self.packet_to_send = Some(meta);
                        return;
                    }
                    Err(err) => {
                        // Some other (fatal?) error occurred; should we panic?
                        // For now, just discard the packet we wanted to send.
                        ringbuf_entry!(Log::SendError(err));
                    }
                }
            }

            // Do we need to send usart data to MGS?
            if let Some(to_flush) = mgs_handler.flush_usart_to_mgs() {
                ringbuf_entry!(Log::SerialConsoleSend {
                    buffered: to_flush.usart.from_rx.len()
                });
                let meta = self.build_serial_console_packet(to_flush);
                self.packet_to_send = Some(meta);

                // Loop back to send.
                continue;
            }

            // All sending is complete; check for an incoming packet.
            match self.net.recv_packet(
                SOCKET,
                LargePayloadBehavior::Discard,
                self.rx_buf,
            ) {
                Ok(meta) => {
                    self.handle_received_packet(meta, mgs_handler);
                }
                Err(RecvError::QueueEmpty) => {
                    return;
                }
                Err(RecvError::NotYours | RecvError::Other) => panic!(),
            }
        }
    }

    fn build_serial_console_packet(
        &mut self,
        to_flush: UsartFlush<'_>,
    ) -> UdpMetadata {
        let message = SpMessage {
            version: gateway_messages::version::V1,
            kind: SpMessageKind::SerialConsole {
                component: SpComponent::SP3_HOST_CPU,
                offset: to_flush.usart.from_rx_offset,
            },
        };

        let (from_rx0, from_rx1) = to_flush.usart.from_rx.as_slices();
        let (n, written) = gateway_messages::serialize_with_trailing_data(
            self.tx_buf,
            &message,
            &[from_rx0, from_rx1],
        );

        // Note: We do not wait for an ack from MGS after sending this data; we
        // hope it receives it, but if not, it's lost. We don't have the buffer
        // space to keep a bunch of data around waiting for acks, and in
        // practice we don't expect lost packets to be a problem.
        to_flush.usart.drain_flushed_data(written);

        UdpMetadata {
            addr: Address::Ipv6(to_flush.destination.ip.into()),
            port: to_flush.destination.port,
            size: n as u32,
            vid: vlan_id_from_sp_port(to_flush.port),
        }
    }

    fn handle_received_packet(
        &mut self,
        mut meta: UdpMetadata,
        mgs_handler: &mut MgsHandler,
    ) {
        ringbuf_entry!(Log::Rx(meta));

        let Address::Ipv6(addr) = meta.addr;
        let sender = gateway_messages::sp_impl::SocketAddrV6 {
            ip: addr.into(),
            port: meta.port,
        };

        // Hand off to `sp_impl` to handle deserialization, calling our
        // `MgsHandler` implementation, and serializing the response we should
        // send into `self.tx_buf`.
        match sp_impl::handle_message(
            sender,
            sp_port_from_udp_metadata(&meta),
            &self.rx_buf[..meta.size as usize],
            mgs_handler,
            &mut self.tx_buf,
        ) {
            Ok(n) => {
                meta.size = n as u32;
                assert!(self.packet_to_send.is_none());
                self.packet_to_send = Some(meta);
            }
            Err(err) => ringbuf_entry!(Log::DispatchError(err)),
        }
    }
}

fn sp_port_from_udp_metadata(meta: &UdpMetadata) -> SpPort {
    use task_net_api::VLAN_RANGE;
    assert!(VLAN_RANGE.contains(&meta.vid));
    assert_eq!(VLAN_RANGE.len(), 2);

    match meta.vid - VLAN_RANGE.start {
        0 => SpPort::One,
        1 => SpPort::Two,
        _ => unreachable!(),
    }
}

fn vlan_id_from_sp_port(port: SpPort) -> u16 {
    use task_net_api::VLAN_RANGE;
    assert_eq!(VLAN_RANGE.len(), 2);

    match port {
        SpPort::One => VLAN_RANGE.start,
        SpPort::Two => VLAN_RANGE.start + 1,
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

/// For our buffers for interacting with the USART FIFO, we want `Deque`s rather
/// than `ArrayVec`s to allow (relatively) cheaply popping data off the front as
/// its transferred in/out of the FIFO.
fn claim_uart_bufs_static(
) -> [&'static mut Deque<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>; 2] {
    use core::sync::atomic::{AtomicBool, Ordering};
    static mut UART_RX_BUF: Deque<
        u8,
        { gateway_messages::MAX_SERIALIZED_SIZE },
    > = Deque::new();
    static mut UART_TX_BUF: Deque<
        u8,
        { gateway_messages::MAX_SERIALIZED_SIZE },
    > = Deque::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `UART_{RX,TX}_BUF`, means that these references can't be aliased by any
    // other reference in the program.
    [unsafe { &mut UART_RX_BUF }, unsafe { &mut UART_TX_BUF }]
}

/// Grabs reference to a static array sized to hold an incoming message. Can
/// only be called once!
fn claim_net_bufs_static() -> &'static mut [SerializedMessageBuf; 2] {
    mutable_statics! {
        static mut BUFS: [SerializedMessageBuf; 2] =
            [[0; gateway_messages::MAX_SERIALIZED_SIZE]; _];
    }
}
