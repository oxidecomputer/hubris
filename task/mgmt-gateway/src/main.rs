// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_stm32h7_usart::Usart;
use gateway_messages::{
    sp_impl, sp_impl::Error as MgsDispatchError, sp_impl::SocketAddrV6,
    IgnitionCommand, Request, SerialConsole, SerializedSize, SpMessage,
    SpMessageKind, SpPort,
};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use task_net_api::{
    Address, LargePayloadBehavior, Net, RecvError, SendError, SocketName,
    UdpMetadata,
};
use tinyvec::ArrayVec;
use userlib::{
    sys_get_timer, sys_irq_control, sys_recv_closed, sys_set_timer, task_slot,
    TaskId, UnwrapLite,
};

mod mgs_handler;

use self::mgs_handler::MgsHandler;

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
    SerialConsoleSend { len: u16 },
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
    SerialConsoleWrite {
        length: u16,
    },
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
    let usart = UsartHandler::new(configure_usart());
    let mut mgs_handler = MgsHandler::new(usart);
    let mut net_handler = NetHandler::new(claim_request_buf_static());

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
    to_tx: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
    from_rx: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
    from_rx_flush_deadline: Option<u64>,
}

impl UsartHandler {
    fn new(usart: Usart) -> Self {
        Self {
            usart,
            to_tx: ArrayVec::default(),
            from_rx: ArrayVec::default(),
            from_rx_flush_deadline: None,
        }
    }

    fn tx_buffer_remaining_capacity(&self) -> usize {
        self.to_tx.capacity() - self.to_tx.len()
    }

    fn tx_buffer_append(&mut self, data: &[u8]) {
        if self.to_tx.is_empty() {
            self.usart.enable_tx_fifo_empty_interrupt();
        }
        self.to_tx.extend_from_slice(data);
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

    fn run_until_blocked(&mut self) {
        // Transmit as much as we have and can.
        let mut n = 0;
        for &b in &self.to_tx {
            if self.usart.try_tx_push(b) {
                n += 1;
            } else {
                break;
            }
        }

        // Clean up / ringbuf debug log after transmitting.
        if n > 0 {
            ringbuf_entry!(Log::UsartTx { num_bytes: n });
            self.to_tx.drain(..n);
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
        }

        // Recv as much as we have space for.
        let mut n = 0;
        let mut available_rx_space =
            self.from_rx.capacity() - self.from_rx.len();

        while available_rx_space > 0 {
            match self.usart.try_rx_pop() {
                Some(b) => {
                    self.from_rx.push(b);
                    n += 1;
                    available_rx_space -= 1;
                }
                None => break,
            }
        }

        if n > 0 {
            ringbuf_entry!(Log::UsartRx { num_bytes: n });

            if self.from_rx_flush_deadline.is_none() {
                let deadline =
                    sys_get_timer().now + SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS;
                self.from_rx_flush_deadline = Some(deadline);
                sys_set_timer(Some(deadline), TIMER_IRQ);
            }
        }
    }
}

struct NetHandler {
    net: Net,
    tx_buf: [u8; SpMessage::MAX_SIZE],
    rx_buf: &'static mut [u8; Request::MAX_SIZE],
    packet_to_send: Option<UdpMetadata>,
}

impl NetHandler {
    fn new(rx_buf: &'static mut [u8; Request::MAX_SIZE]) -> Self {
        Self {
            net: Net::from(NET.get_task_id()),
            tx_buf: [0; SpMessage::MAX_SIZE],
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
            if let Some((serial_console_packet, mgs_addr, sp_port)) =
                mgs_handler.flush_usart_to_mgs()
            {
                ringbuf_entry!(Log::SerialConsoleSend {
                    len: serial_console_packet.len
                });
                let meta = self.build_serial_console_packet(
                    serial_console_packet,
                    mgs_addr,
                    sp_port,
                );
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
        packet: SerialConsole,
        mgs_addr: SocketAddrV6,
        sp_port: SpPort,
    ) -> UdpMetadata {
        let message = SpMessage {
            version: gateway_messages::version::V1,
            kind: SpMessageKind::SerialConsole(packet),
        };

        // We know `self.tx_buf` is large enough for any `SpMessage`, so we can
        // unwrap this `serialize()`.
        let n = gateway_messages::serialize(&mut self.tx_buf, &message)
            .unwrap_lite();

        UdpMetadata {
            addr: Address::Ipv6(mgs_addr.ip.into()),
            port: mgs_addr.port,
            size: n as u32,
            vid: vlan_id_from_sp_port(sp_port),
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

/// Grabs reference to a static array sized to hold a `Request`. Can only be
/// called once!
fn claim_request_buf_static() -> &'static mut [u8; Request::MAX_SIZE] {
    mutable_statics! {
        static mut REQUEST_BUF: [u8; Request::MAX_SIZE] = [0; _];
    }
}
