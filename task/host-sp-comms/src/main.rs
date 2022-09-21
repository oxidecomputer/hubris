// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::ops::Range;

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_usart::Usart;
use heapless::Vec;
use host_sp_messages::{
    Bsu, DecodeFailureReason, Header, HostToSp, HubpackError, SpToHost, Status,
    MAX_MESSAGE_SIZE,
};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{
    sys_irq_control, sys_recv_closed, task_slot, TaskId, UnwrapLite,
};

task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Trace {
    None,
    UartTx(u8),
    UartTxFull,
    UartRx(u8),
    UartRxOverrun,
    Notification(u32),
}

ringbuf!(Trace, 64, Trace::None);

/// Notification bit for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1 << 0;

/// Notification bit for Jefe notifying us of state changes; must match Jefe's
/// `on-state-change` config for us in app.toml.
const JEFE_STATE_CHANGE_IRQ: u32 = 1 << 1;

/// We set the high bit of the sequence number before replying to host requests.
const SEQ_REPLY: u64 = 0x8000_0000_0000_0000;

/// We wrap host/sp messages in corncobs; derive our max packet length from the
/// max unwrapped message length.
const MAX_PACKET_SIZE: usize = corncobs::max_encoded_len(MAX_MESSAGE_SIZE);

#[export_name = "main"]
fn main() -> ! {
    let mut server =
        ServerImpl::claim_static_resources(Status::SP_TASK_RESTARTED);

    sys_irq_control(USART_IRQ, true);

    loop {
        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let note = sys_recv_closed(
            &mut [],
            USART_IRQ | JEFE_STATE_CHANGE_IRQ,
            TaskId::KERNEL,
        )
        .unwrap_lite()
        .operation;
        ringbuf_entry!(Trace::Notification(note));

        server.handle_notification(note);
    }
}

struct ServerImpl {
    uart: Usart,
    tx_msg_buf: &'static mut [u8; MAX_MESSAGE_SIZE],
    tx_pkt_buf: &'static mut [u8; MAX_PACKET_SIZE],
    tx_pkt_to_write: Range<usize>,
    rx_buf: &'static mut Vec<u8, MAX_PACKET_SIZE>,
    status: Status,
}

impl ServerImpl {
    fn claim_static_resources(status: Status) -> Self {
        let (tx_msg_buf, tx_pkt_buf) = mutable_statics! {
                static mut UART_TX_MSG_BUF: [u8; MAX_MESSAGE_SIZE] = [0; _];
                static mut UART_TX_PKT_BUF: [u8; MAX_PACKET_SIZE] = [0; _];
        };
        Self {
            uart: configure_uart_device(),
            tx_msg_buf,
            tx_pkt_buf,
            tx_pkt_to_write: 0..0,
            rx_buf: claim_uart_rx_buf(),
            status,
        }
    }

    fn handle_usart_notification(&mut self) {
        'tx: loop {
            // Do we have data to transmit? If so, write as much as we can until
            // either the fifo fills (in which case we return before trying to
            // receive more) or we finish flushing.
            while !self.tx_pkt_to_write.is_empty() {
                if try_tx_push(
                    &self.uart,
                    self.tx_pkt_buf[self.tx_pkt_to_write.start],
                ) {
                    self.tx_pkt_to_write.start += 1;
                } else {
                    return;
                }
            }

            // We're done flushing data; disable the tx fifo interrupt.
            self.uart.disable_tx_fifo_empty_interrupt();

            // Clear any RX overrun errors. If we hit this, we will likely fail
            // to decode the next message from the host, which will cause us to
            // send a `DecodeFailure` response.
            if self.uart.check_and_clear_rx_overrun() {
                ringbuf_entry!(Trace::UartRxOverrun);
            }

            // Receive until there's no more data or we get a 0x00, signifying
            // the end of a corncobs packet.
            while let Some(byte) = self.uart.try_rx_pop() {
                ringbuf_entry!(Trace::UartRx(byte));

                if byte == 0x00 {
                    // Process message and populate our response into
                    // `tx_msg_buf`.
                    let msg_len = process_message(
                        &mut self.status,
                        self.tx_msg_buf,
                        self.rx_buf.as_mut(),
                    );
                    self.rx_buf.clear();

                    // Encode our outgoing packet.
                    let pkt_len = corncobs::encode_buf(
                        &self.tx_msg_buf[..msg_len],
                        &mut self.tx_pkt_buf[..],
                    );
                    self.tx_pkt_to_write = 0..pkt_len;

                    // Enable tx fifo interrupts, and immediately start trying
                    // to send our response.
                    self.uart.enable_tx_fifo_empty_interrupt();
                    continue 'tx;
                } else if self.rx_buf.push(byte).is_err() {
                    // Message overflow - nothing we can do here except
                    // discard data. We'll drop this byte and wait til we
                    // see a 0 to respond, at which point our
                    // deserialization will presumably fail and we'll send
                    // back an error. Should we record that we overflowed
                    // here?
                }
            }

            // We received everything we could out of the rx fifo and we have
            // nothing to send; we're done.
            return;
        }
    }
}

// approximately idol_runtime::NotificationHandler, in anticipation of
// eventually having an idol interface (at least for mgmt-gateway to give us
// host phase 2 data)
impl ServerImpl {
    fn handle_notification(&mut self, bits: u32) {
        if bits & USART_IRQ != 0 {
            self.handle_usart_notification();
            sys_irq_control(USART_IRQ, true);
        }

        if bits & JEFE_STATE_CHANGE_IRQ != 0 {}
    }
}

// wrapper around `usart.try_tx_push()` that registers the result in our
// ringbuf
fn try_tx_push(uart: &Usart, val: u8) -> bool {
    let ret = uart.try_tx_push(val);
    if ret {
        ringbuf_entry!(Trace::UartTx(val));
    } else {
        ringbuf_entry!(Trace::UartTxFull);
    }
    ret
}

fn populate_with_decode_error(
    out: &mut [u8; MAX_MESSAGE_SIZE],
    reason: DecodeFailureReason,
) -> usize {
    let header = Header {
        magic: host_sp_messages::MAGIC,
        version: host_sp_messages::version::V1,
        // We failed to decode, so don't know the sequence number.
        sequence: 0xffff_ffff_ffff_ffff,
    };
    let response = SpToHost::DecodeFailure(reason);

    // Serializing can only fail if we pass unexpected types as `response`, but
    // we're using `SpToHost`, so it cannot fail.
    host_sp_messages::serialize(out, &header, &response, &[])
        .unwrap_lite()
        .0
}

fn process_message(
    status: &mut Status,
    out: &mut [u8; MAX_MESSAGE_SIZE],
    frame: &mut [u8],
) -> usize {
    let deframed = match corncobs::decode_in_place(frame) {
        Ok(n) => &frame[..n],
        Err(_) => {
            return populate_with_decode_error(out, DecodeFailureReason::Cobs)
        }
    };

    let (mut header, request, data) = match host_sp_messages::deserialize::<
        HostToSp,
    >(deframed)
    {
        Ok((header, request, data)) => (header, request, data),
        Err(HubpackError::Custom) => {
            return populate_with_decode_error(out, DecodeFailureReason::Crc)
        }
        Err(_) => {
            return populate_with_decode_error(
                out,
                DecodeFailureReason::Deserialize,
            )
        }
    };

    if header.magic != host_sp_messages::MAGIC {
        return populate_with_decode_error(
            out,
            DecodeFailureReason::MagicMismatch,
        );
    }

    if header.version != host_sp_messages::version::V1 {
        return populate_with_decode_error(
            out,
            DecodeFailureReason::VersionMismatch,
        );
    }

    if header.sequence & SEQ_REPLY != 0 {
        return populate_with_decode_error(
            out,
            DecodeFailureReason::SequenceInvalid,
        );
    }

    let mut response_data: &[u8] = &[];
    let response = match request {
        HostToSp::_Unused => {
            SpToHost::DecodeFailure(DecodeFailureReason::Deserialize)
        }
        HostToSp::RequestReboot => {
            // TODO reboot host
            SpToHost::Ack
        }
        HostToSp::RequestPowerOff => {
            // TODO power off host
            SpToHost::Ack
        }
        HostToSp::GetBootStorageUnit => {
            // TODO how do we know the real answer?
            SpToHost::BootStorageUnit(Bsu::A)
        }
        HostToSp::GetIdentity => {
            // TODO how do we get our real identity?
            SpToHost::Identity {
                model: 1,
                revision: 2,
                serial: *b"fake-serial",
            }
        }
        HostToSp::GetMacAddresses => {
            // TODO where do we get host MAC addrs?
            SpToHost::MacAddresses {
                base: [0; 6],
                count: 1,
                stride: 1,
            }
        }
        HostToSp::HostBootFailure { .. } => {
            // TODO what do we do in reaction to this?
            SpToHost::Ack
        }
        HostToSp::HostPanic { .. } => {
            // TODO what do we do in reaction to this?
            SpToHost::Ack
        }
        HostToSp::GetStatus => SpToHost::Status(*status),
        HostToSp::ClearStatus { mask } => {
            *status &= Status::from_bits_truncate(!mask);
            SpToHost::Ack
        }
        HostToSp::GetAlert { mask: _ } => {
            // TODO define alerts
            SpToHost::Alert { action: 0 }
        }
        HostToSp::RotRequest => {
            // TODO forward request to RoT; for now just echo
            response_data = data;
            SpToHost::RotResponse
        }
        HostToSp::RotAddHostMeasurements => {
            // TODO forward request to RoT
            SpToHost::Ack
        }
        HostToSp::GetPhase2Data { start, count: _ } => {
            // TODO forward real data
            response_data = b"hello world";
            SpToHost::Phase2Data { start }
        }
    };

    // We set the high bit of the sequence number before responding.
    header.sequence |= SEQ_REPLY;

    // TODO this can panic if `response_data` is too large; where do we check it
    // before sending a response?
    host_sp_messages::serialize(out, &header, &response, response_data)
        .unwrap_lite()
        .0
}

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device() -> Usart {
    use drv_usart::device;
    use drv_usart::drv_stm32xx_sys_api::*;

    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 100_000_000;

    // For gimlet, we only expect baud rate 3 Mbit, uart7, with hardware flow
    // control enabled. We could expand our cargo features to cover other cases
    // as needed. Currently, failing to enable any of those three features will
    // cause a compilation error.
    #[cfg(feature = "baud_rate_3M")]
    const BAUD_RATE: u32 = 3_000_000;

    #[cfg(feature = "hardware_flow_control")]
    let hardware_flow_control = true;

    cfg_if::cfg_if! {
        if #[cfg(feature = "uart7")] {
            const PINS: &[(PinSet, Alternate)] = {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "hardware_flow_control")] {
                        &[(
                            Port::E.pin(7).and_pin(8).and_pin(9).and_pin(10),
                            Alternate::AF7
                        )]
                    } else {
                        compile_error!("hardware_flow_control should be enabled");
                    }
                }
            };
            let usart = unsafe { &*device::UART7::ptr() };
            let peripheral = Peripheral::Uart7;
            let pins = PINS;
        } else {
            compile_error!("no usartX/uartX feature specified");
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

fn claim_uart_rx_buf() -> &'static mut Vec<u8, MAX_PACKET_SIZE> {
    use core::sync::atomic::{AtomicBool, Ordering};

    static mut UART_RX_BUF: Vec<u8, MAX_PACKET_SIZE> = Vec::new();

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
