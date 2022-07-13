// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_stm32h7_usart::Usart;
use gateway_messages::sp_impl;
use gateway_messages::sp_impl::Error as MgsDispatchError;
use gateway_messages::sp_impl::SerialConsolePacketizer;
use gateway_messages::sp_impl::SocketAddrV6;
use gateway_messages::IgnitionCommand;
use gateway_messages::Request;
use gateway_messages::SerialConsole;
use gateway_messages::SerializedSize;
use gateway_messages::SpComponent;
use gateway_messages::SpMessage;
use gateway_messages::SpMessageKind;
use gateway_messages::SpPort;
use ringbuf::ringbuf;
use ringbuf::ringbuf_entry;
use task_net_api::Address;
use task_net_api::Net;
use task_net_api::NetError;
use task_net_api::SocketName;
use task_net_api::UdpMetadata;
use tinyvec::ArrayVec;
use userlib::sys_irq_control;
use userlib::sys_recv_closed;
use userlib::task_slot;
use userlib::TaskId;
use userlib::UnwrapLite;

mod mgs_handler;

use self::mgs_handler::MgsHandler;

task_slot!(NET, net);
task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Log {
    Empty,
    Rx(UdpMetadata),
    DispatchError(MgsDispatchError),
    SendError(NetError),
    MgsMessage(MgsMessage),
    UsartTx { num_bytes: usize },
    UsartTxFull { remaining: usize },
    UsartRx { num_bytes: usize },
    UsartRxOverrun,
    SerialConsoleSend { len: u16 },
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
}

ringbuf!(Log, 16, Log::Empty);

// Must match app.toml!
const NET_IRQ: u32 = 1;
const USART_IRQ: u32 = 2;

const SOCKET: SocketName = SocketName::mgmt_gateway;

#[export_name = "main"]
fn main() {
    let mut usart_handler = UsartHandler::new(configure_usart());
    let mut net_handler = NetHandler::default();

    // Enbale USART interrupts.
    sys_irq_control(USART_IRQ, true);

    let mut note = NET_IRQ;
    loop {
        if (note & USART_IRQ) != 0 {
            usart_handler.run_until_blocked();
            sys_irq_control(USART_IRQ, true);
        }

        if (note & NET_IRQ) != 0 || usart_handler.should_flush_to_mgs() {
            net_handler.run_until_blocked(&mut usart_handler);
        }

        note = sys_recv_closed(&mut [], NET_IRQ | USART_IRQ, TaskId::KERNEL)
            .unwrap_lite()
            .operation;
    }
}

struct UsartHandler {
    usart: Usart,
    to_tx: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
    from_rx: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
}

impl UsartHandler {
    fn new(usart: Usart) -> Self {
        Self {
            usart,
            to_tx: ArrayVec::default(),
            from_rx: ArrayVec::default(),
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
        // TODO also add time check
        self.from_rx.len() > 10
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
        }
    }
}

struct NetHandler {
    net: Net,
    rx_buf: [u8; Request::MAX_SIZE],
    tx_buf: [u8; SpMessage::MAX_SIZE],
    serial_console_packetizer: SerialConsolePacketizer,
    attached_serial_console_mgs: Option<(SocketAddrV6, SpPort)>,
    packet_to_send: Option<UdpMetadata>,
}

impl Default for NetHandler {
    fn default() -> Self {
        Self {
            net: Net::from(NET.get_task_id()),
            rx_buf: [0; Request::MAX_SIZE],
            tx_buf: [0; SpMessage::MAX_SIZE],
            serial_console_packetizer: SerialConsolePacketizer::new(
                // TODO should we remove the "component" from the serial console
                // MGS API? Any chance we ever want to support multiple "serial
                // console"s?
                SpComponent::try_from("sp3").unwrap_lite(),
            ),
            attached_serial_console_mgs: None,
            packet_to_send: None,
        }
    }
}

impl NetHandler {
    fn run_until_blocked(&mut self, usart: &mut UsartHandler) {
        loop {
            // Try to send first.
            if let Some(meta) = self.packet_to_send.take() {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_buf[..meta.size as usize],
                ) {
                    Ok(()) => (),
                    Err(err) => {
                        ringbuf_entry!(Log::SendError(err));

                        // Re-enqueue packet and return; we'll wait to be awoken
                        // by the net task when it has room for us to send.
                        //
                        // TODO Should we drop packets for non-"out of space"
                        // errors? Need to fix net task returning QueueEmpty for
                        // arbitrary errors.
                        self.packet_to_send = Some(meta);
                        return;
                    }
                }
            }

            // Does our USART have data buffered that we should send?
            if usart.should_flush_to_mgs() {
                if let Some((mgs_addr, sp_port)) =
                    self.attached_serial_console_mgs
                {
                    let (serial_console_packet, leftover) = self
                        .serial_console_packetizer
                        .first_packet(&usart.from_rx);

                    // Based on the size of `usart.from_rx`, we should never have
                    // any leftover data (it holds at most one packet worth).
                    assert!(leftover.is_empty());
                    usart.from_rx.clear();

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
                } else {
                    // We have no attached MGS instance; discard serial port
                    // data.
                    usart.from_rx.clear();
                }
            }

            // All sending is complete; check for an incoming packet.
            match self.net.recv_packet(SOCKET, &mut self.rx_buf) {
                Ok(meta) => {
                    self.handle_received_packet(meta, usart);
                }
                Err(NetError::QueueEmpty) => {
                    return;
                }
                Err(
                    NetError::NotYours
                    | NetError::InvalidVLan
                    | NetError::QueueFull
                    | NetError::Other,
                ) => panic!(),
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
        usart: &mut UsartHandler,
    ) {
        ringbuf_entry!(Log::Rx(meta));

        let addr = match meta.addr {
            task_net_api::Address::Ipv6(addr) => addr,
        };
        let sender = gateway_messages::sp_impl::SocketAddrV6 {
            ip: addr.into(),
            port: meta.port,
        };

        // Hand off to `sp_impl` to handle deserialization, calling our
        // `MgsHandler` implementation, and serializing the response we should
        // send into `self.tx_buf`.
        let mut mgs_handler = MgsHandler::new(usart);
        match sp_impl::handle_message(
            sender,
            sp_port_from_udp_metadata(&meta),
            &self.rx_buf[..meta.size as usize],
            &mut mgs_handler,
            &mut self.tx_buf,
        ) {
            Ok(n) => {
                meta.size = n as u32;
                assert!(self.packet_to_send.is_none());
                self.packet_to_send = Some(meta);
                if let Some(mgs) = mgs_handler.attached_serial_console_mgs() {
                    self.attached_serial_console_mgs = Some(mgs);
                }
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
