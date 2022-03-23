// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_usart::Usart;
use gateway_messages::sp_impl::SerialConsolePacketizer;
use gateway_messages::sp_impl::SpHandler;
use gateway_messages::sp_impl::SpServer;
use gateway_messages::Request;
use gateway_messages::ResponseError;
use gateway_messages::SerialConsole;
use gateway_messages::SerializedSize;
use gateway_messages::SpComponent;
use gateway_messages::SpMessage;
use gateway_messages::SpMessageKind;
use gateway_messages::SpState;
use ringbuf::ringbuf;
use ringbuf::ringbuf_entry;
use task_net_api::*;
use tinyvec::ArrayVec;
use unwrap_lite::UnwrapLite;
use userlib::*;

task_slot!(NET, net);
task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum LogPacket {
    Empty,
    Rx { length: usize },
    Tx { length: usize },
    Err(gateway_messages::sp_impl::Error),
}
ringbuf!(PACKETS, LogPacket, 16, LogPacket::Empty);

#[derive(Debug, Clone, Copy, PartialEq)]
enum LogUart {
    Tx(u8),
    TxFull(usize),
    Rx(u8),
    RxOverrun,
}
ringbuf!(UART, LogUart, 64, LogUart::Rx(0));

/// Notification mask for net IRQ; must match configuration in app.toml.
const NET_IRQ: u32 = 0x01;

/// Notification mask for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 0x02;

/// Notification mask for our internal timer; must not conflict with above IRQs.
const TIMER_IRQ: u32 = 0x04;

/// TODO DOCS
/// flush a serial console packet after N millis or our buffer is X% full
const SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS: u64 = 500;

#[export_name = "main"]
fn main() -> ! {
    // TODO rename socket
    const SOCKET: SocketName = SocketName::echo;

    const NET_BUF_LEN: usize =
        const_max(Request::MAX_SIZE, SpMessage::MAX_SIZE);

    let uart = configure_uart_device();

    let net = NET.get_task_id();
    let net = Net::from(net);

    let mut net_buf = [0u8; NET_BUF_LEN];
    let mut server = SpServer::default();
    let mut handler = Handler::new(&uart);
    let mut mgs_addr: Option<UdpMetadata> = None;

    // TODO component?
    let mut serial_console_packetizer = SerialConsolePacketizer::new(
        SpComponent::try_from("sp3").unwrap_lite(),
    );

    // enable USART interrupts
    sys_irq_control(USART_IRQ, true);

    loop {
        // wait for an interrupt (either from the net task or the uart)
        let note = sys_recv_closed(
            &mut [],
            NET_IRQ | USART_IRQ | TIMER_IRQ,
            TaskId::KERNEL,
        )
        .unwrap_lite()
        .operation;

        if (note & NET_IRQ) != 0 {
            match net.recv_packet(SOCKET, &mut net_buf) {
                Ok(mut meta) => {
                    mgs_addr = Some(meta);
                    let rx = &net_buf[..meta.size as usize];
                    if let Some(tx_data) = handler.recv_packet(rx, &mut server)
                    {
                        meta.size = tx_data.len() as u32;

                        // TODO don't panic on failure?
                        net.send_packet(SOCKET, meta, tx_data).unwrap_lite();
                        ringbuf_entry!(
                            PACKETS,
                            LogPacket::Tx {
                                length: tx_data.len()
                            }
                        );
                    }
                }
                Err(NetError::QueueEmpty) => {
                    // spurious wakeup; nothing to do
                }
                Err(NetError::NotYours) => panic!(),
            }
        }

        if (note & USART_IRQ) != 0 {
            handler.serial_console_tx();
            handler.serial_console_rx();
            sys_irq_control(USART_IRQ, true);
        }

        // See if we need to send a serial console packet; this can happen if we
        // hit our timer or if our buffer is getting full
        let timer_elapsed = (note & TIMER_IRQ) != 0;
        handler.try_serial_console_send(timer_elapsed, |data| {
            // we have serial console data to send but don't know who
            // to send it to; claim we sent it which results in it being
            // discarded
            let mut meta = match mgs_addr {
                Some(meta) => meta,
                None => return true,
            };

            // packetizer gives us an iterator, but due to the size of handler's
            // buf we know there will only be a single packet. we're explicit
            // about this instead of looping over the iter and assuming it only
            // loops once
            let mut packet_iter = serial_console_packetizer.packetize(data);
            let packet = packet_iter.next().unwrap_lite();
            if packet_iter.next().is_some() {
                panic!();
            }

            let message = SpMessage {
                version: gateway_messages::version::V1,
                kind: SpMessageKind::SerialConsole(packet),
            };

            let n = gateway_messages::serialize(&mut net_buf[..], &message)
                .unwrap_lite();
            let tx = &net_buf[..n];
            meta.size = n as u32;

            // TODO don't panic on failure?
            net.send_packet(SOCKET, meta, tx).unwrap_lite();

            // packet has been sent; tell handler to clear its buffer
            true
        });
    }
}

const fn const_max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

struct Handler<'a> {
    uart: &'a Usart,
    serial_console_out: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
    serial_console_in: ArrayVec<[u8; SerialConsole::MAX_DATA_PER_PACKET]>,
}

impl<'a> Handler<'a> {
    fn new(uart: &'a Usart) -> Self {
        Self {
            uart,
            serial_console_out: ArrayVec::default(),
            serial_console_in: ArrayVec::default(),
        }
    }

    fn serial_console_tx(&mut self) {
        let mut n = 0;
        for &byte in &self.serial_console_out {
            if self.uart.try_tx_push(byte) {
                n += 1;
                ringbuf_entry!(UART, LogUart::Tx(byte));
            } else {
                ringbuf_entry!(
                    UART,
                    LogUart::TxFull(self.serial_console_out.len() - n)
                );
                break;
            }
        }

        if n > 0 {
            self.serial_console_out.drain(..n);
        }

        if self.serial_console_out.is_empty() {
            self.uart.disable_tx_fifo_empty_interrupt();
        }
    }

    fn serial_console_rx(&mut self) {
        if self.uart.check_and_clear_rx_overrun() {
            ringbuf_entry!(UART, LogUart::RxOverrun);
        }

        let mut read_at_least_one_byte = false;
        while self.serial_console_in.len() < self.serial_console_in.capacity() {
            match self.uart.try_rx_pop() {
                Some(byte) => {
                    ringbuf_entry!(UART, LogUart::Rx(byte));
                    read_at_least_one_byte = true;

                    // we know there's space due to the `is_empty` check; use
                    // `try_push` instead of `push` for a lighter unwrap
                    match self.serial_console_in.try_push(byte) {
                        Some(_) => panic!(),
                        None => (),
                    }
                }
                None => break,
            }
        }

        if read_at_least_one_byte {
            sys_set_timer(
                Some(sys_get_timer().now + SERIAL_CONSOLE_FLUSH_TIMEOUT_MILLIS),
                TIMER_IRQ,
            );
        }
    }

    fn try_serial_console_send<F>(&mut self, timer_elapsed: bool, try_send: F)
    where
        F: FnOnce(&[u8]) -> bool,
    {
        // flush and send a packet if it's been long enough or if our buffer is
        // approaching full, which we'll define as 7/8ths for now
        if timer_elapsed
            || self.serial_console_in.len()
                > (self.serial_console_in.capacity() * 7) / 8
        {
            if try_send(&self.serial_console_in) {
                self.serial_console_in.clear();
            }
        }
    }

    fn recv_packet<'b>(
        &mut self,
        data: &[u8],
        server: &'b mut SpServer,
    ) -> Option<&'b [u8]> {
        ringbuf_entry!(PACKETS, LogPacket::Rx { length: data.len() });

        match server.dispatch(data, self) {
            Ok(tx) => Some(tx),
            Err(err) => {
                ringbuf_entry!(PACKETS, LogPacket::Err(err));
                None
            }
        }
    }
}

impl SpHandler for Handler<'_> {
    fn ping(&mut self) -> Result<(), ResponseError> {
        Ok(())
    }

    fn ignition_state(
        &mut self,
        _target: u8,
    ) -> Result<gateway_messages::IgnitionState, ResponseError> {
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
    ) -> Result<gateway_messages::BulkIgnitionState, ResponseError> {
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _target: u8,
        _command: gateway_messages::IgnitionCommand,
    ) -> Result<(), ResponseError> {
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn sp_state(&mut self) -> Result<SpState, ResponseError> {
        let mut serial_number = [0; 16];
        // TODO make this less bad
        unsafe {
            // stm32 has a 12-byte unique device ID
            for i in 0..3 {
                let x = *DEVICE_ID.add(i);
                serial_number[4 * i..4 * (i + 1)]
                    .copy_from_slice(&x.to_le_bytes());
            }
        }
        Ok(SpState { serial_number })
    }

    fn serial_console_write(
        &mut self,
        packet: SerialConsole,
    ) -> Result<(), ResponseError> {
        // TODO check packet.component?
        // TODO check packet.offset?

        // convert `packet`'s data into an `ArrayVec` so we can use
        // `try_append()` below
        let mut data = ArrayVec::from(packet.data);
        data.truncate(usize::from(packet.len));

        if self.serial_console_out.try_append(&mut data).is_none() {
            self.uart.enable_tx_fifo_empty_interrupt();
            Ok(())
        } else {
            Err(ResponseError::Busy)
        }
    }
}

// TODO better way to access this?
#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
const DEVICE_ID: *const u32 = 0x1ff1e800 as *const u32;

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device() -> Usart {
    use drv_usart::device;
    use drv_usart::drv_stm32xx_sys_api::*;

    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 100_000_000;

    const BAUD_RATE: u32 = 115_600;

    // From thin air, pluck a pointer to the USART register block.
    //
    // Safety: this is needlessly unsafe in the API. The USART is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let usart = unsafe { &*device::USART3::ptr() };

    Usart::turn_on(
        &Sys::from(SYS.get_task_id()),
        usart,
        Peripheral::Usart3,
        Port::D.pin(8).and_pin(9),
        Alternate::AF7,
        CLOCK_HZ,
        BAUD_RATE,
    )
}
