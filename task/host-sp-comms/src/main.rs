// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

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
enum UartLog {
    None,
    Tx(u8),
    TxFull,
    Rx(u8),
    RxOverrun,
}

ringbuf!(UartLog, 64, UartLog::None);

/// Notification mask for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1;

/// We set the high bit of the sequence number before replying to host requests.
const SEQ_REPLY: u64 = 0x8000_0000_0000_0000;

#[export_name = "main"]
fn main() -> ! {
    let mut server =
        ServerImpl::claim_static_resources(Status::SP_TASK_RESTARTED);

    sys_irq_control(USART_IRQ, true);

    loop {
        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let note =
            sys_recv_closed(&mut [], USART_IRQ, TaskId::KERNEL).unwrap_lite();

        server.handle_notification(note.operation);
    }
}

struct ServerImpl {
    uart: Usart,
    tx_buf: &'static mut [u8; MAX_MESSAGE_SIZE],
    rx_buf: &'static mut Vec<u8, MAX_MESSAGE_SIZE>,
    status: Status,
}

impl ServerImpl {
    fn claim_static_resources(status: Status) -> Self {
        Self {
            uart: configure_uart_device(),
            tx_buf: mutable_statics! {
                static mut UART_TX_BUF: [u8; MAX_MESSAGE_SIZE] = [0; _];
            },
            rx_buf: claim_uart_rx_buf(),
            status,
        }
    }
}

// approximately idol_runtime::NotificationHandler, in anticipation of
// eventually having an idol interface (at least for mgmt-gateway to give us
// host phase 2 data)
impl ServerImpl {
    fn handle_notification(&mut self, _bits: u32) {
        // Clear any RX overrun errors. If we hit this, we will likely fail to
        // decode the next message from the host, which will cause us to send a
        // `DecodeFailure` response.
        if self.uart.check_and_clear_rx_overrun() {
            ringbuf_entry!(UartLog::RxOverrun);
        }

        // This is going to be fixed up with another PR
        #[allow(clippy::never_loop)]
        let maybe_response = 'response: loop {
            // Receive until we find a message delimiter. Since we're using
            // corncobs for framing, we're looking for 0x00.
            while let Some(byte) = self.uart.try_rx_pop() {
                ringbuf_entry!(UartLog::Rx(byte));

                if byte == 0x00 {
                    let n = process_message(
                        &mut self.status,
                        self.tx_buf,
                        self.rx_buf.as_mut(),
                    );
                    self.rx_buf.clear();
                    break 'response Some(&self.tx_buf[..n]);
                } else if self.rx_buf.push(byte).is_err() {
                    // Message overflow - nothing we can do here except
                    // discard data. We'll drop this byte and wait til we
                    // see a 0 to respond, at which point our
                    // deserialization will presumably fail and we'll send
                    // back an error. Should we record that we overflowed
                    // here?
                }
            }

            // RX FIFO is empty and we don't have a complete message.
            break None;
        };

        // Spin here until we're able to flush our entire response out to the TX
        // FIFO. We're not going to attempt to read from the RX FIFO while we're
        // doing this, because the host isn't supposed to be sending us
        // pipelined requests.
        if let Some(unframed_data) = maybe_response {
            let mut iter = corncobs::encode_iter(unframed_data).peekable();

            self.uart.enable_tx_fifo_empty_interrupt();
            while let Some(&b) = iter.peek() {
                if try_tx_push(&self.uart, b) {
                    // Discard the byte we just peeked and successfully inserted
                    // into the TX FIFO.
                    iter.next().unwrap_lite();
                } else {
                    // TX fifo is full; wait for space.
                    sys_irq_control(USART_IRQ, true);
                    let _ = sys_recv_closed(&mut [], USART_IRQ, TaskId::KERNEL);
                }
            }
            self.uart.disable_tx_fifo_empty_interrupt();
        }

        // Re-enable USART interrupts.
        sys_irq_control(USART_IRQ, true);
    }
}

// wrapper around `usart.try_tx_push()` that registers the result in our
// ringbuf
fn try_tx_push(uart: &Usart, val: u8) -> bool {
    let ret = uart.try_tx_push(val);
    if ret {
        ringbuf_entry!(UartLog::Tx(val));
    } else {
        ringbuf_entry!(UartLog::TxFull);
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

fn claim_uart_rx_buf() -> &'static mut Vec<u8, MAX_MESSAGE_SIZE> {
    use core::sync::atomic::{AtomicBool, Ordering};

    static mut UART_RX_BUF: Vec<u8, MAX_MESSAGE_SIZE> = Vec::new();

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
