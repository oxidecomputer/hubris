// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]
#![feature(asm)]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_usart::Usart;
use ringbuf::*;
use tinyvec::{ArrayVec, SliceVec};
use userlib::*;

use corncobs;
use hubpack::{deserialize, serialize, SerializedSize};
use sprockets_common::msgs::{
    RotError, RotOp, RotRequest, RotResponse, RotResult,
};
use sprockets_rot::{RotConfig, RotSprocket};

task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum UartLog {
    Tx(u8),
    TxFull,
    Rx(u8),
    RxOverrun,
    ValidReq,
    JunkReq,
    AboutToDecode,
    TooLarge,
}

ringbuf!(UartLog, 64, UartLog::Rx(0));

/// Notification mask for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let uart = configure_uart_device();
    let mut req_buf_backing =
        [0u8; corncobs::max_encoded_len(RotRequest::MAX_SIZE)];

    // This is not a COBS encoded buffer. We are using corncobs::encode_iter to
    // write bytes over tx. Therefore there is no need for the extra space
    // required for COBS.
    let mut rsp_buf_backing = [0u8; RotRequest::MAX_SIZE];

    let mut rsp_buf_encoded_backing =
        [0u8; corncobs::max_encoded_len(RotResponse::MAX_SIZE)];

    // We use a SliceVec instead of an ArrayVec since the capacities returned
    // from corncobs::max_encoded_len are not necessarily fewer than 32 or
    // powers of two <= 4096.
    // See https://docs.rs/tinyvec/latest/tinyvec/trait.Array.html
    let mut req_buf = SliceVec::from(&mut req_buf_backing);
    req_buf.set_len(0);
    let mut rsp_buf = SliceVec::from(&mut rsp_buf_backing);

    // TODO: Prevent the need for this by using corncobs::encode_iter
    let mut rsp_buf_encoded = SliceVec::from(&mut rsp_buf_encoded_backing);

    let config = RotConfig::bootstrap_for_testing();
    let mut sprocket = RotSprocket::new(config);

    let mut need_to_tx: Option<(&SliceVec<u8>, usize)> = None;

    sys_irq_control(USART_IRQ, true);

    loop {
        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let _ = sys_recv_closed(&mut [], USART_IRQ, TaskId::KERNEL);

        // Walk through our tx state machine to handle echoing lines back; note
        // that many of these cases intentionally break after refilling
        // `need_to_tx` if we fill the TX fifo.
        while need_to_tx.is_some() {
            let (buf, pos) = need_to_tx.as_mut().unwrap();
            if buf.len() == *pos {
                need_to_tx = None;
                break;
            }
            if !try_tx_push(&uart, buf[*pos]) {
                break;
            } else {
                *pos += 1;
            }
        }

        // if we filled the tx fifo but still have more to send, reenable our
        // interrupts and loop before we try to rx more
        if need_to_tx.is_some() {
            sys_irq_control(USART_IRQ, true);
            continue;
        }

        // all tx is done; now pull from the rx fifo
        if uart.check_and_clear_rx_overrun() {
            ringbuf_entry!(UartLog::RxOverrun);
        }

        while let Some(byte) = uart.try_rx_pop() {
            ringbuf_entry!(UartLog::Rx(byte));

            req_buf.push(byte);

            // Keep looking for 0, as we are using COBS for framing.
            if byte == 0 {
                ringbuf_entry!(UartLog::AboutToDecode);
                handle_req(&mut sprocket, &mut req_buf, &mut rsp_buf);
                uart.enable_tx_fifo_empty_interrupt();
                rsp_buf_encoded.set_len(rsp_buf_encoded.capacity());
                let size = corncobs::encode_buf(
                    rsp_buf.as_slice(),
                    rsp_buf_encoded.as_mut_slice(),
                );
                rsp_buf_encoded.set_len(size);
                need_to_tx = Some((&rsp_buf_encoded, 0));
                break;
            }

            // Max request size exceeded
            if req_buf.len() == req_buf.capacity() {
                ringbuf_entry!(UartLog::TooLarge);
                bad_encoding_rsp(&mut rsp_buf);
                uart.enable_tx_fifo_empty_interrupt();

                rsp_buf_encoded.set_len(rsp_buf_encoded.capacity());
                let size = corncobs::encode_buf(
                    rsp_buf.as_slice(),
                    rsp_buf_encoded.as_mut_slice(),
                );
                rsp_buf_encoded.set_len(size);
                need_to_tx = Some((&mut rsp_buf_encoded, 0));
                break;
            }
        }

        // re-enable USART interrupts
        sys_irq_control(USART_IRQ, true);

        // Uncomment this to artifically slow down the task to make it easier to
        // see RxOverrun errors
        //hl::sleep_for(200);
    }
}

fn handle_req(
    sprocket: &mut RotSprocket,
    req_buf: &mut SliceVec<u8>,
    rsp_buf: &mut SliceVec<u8>,
) {
    if let Err(_) = decode_frame(req_buf) {
        bad_encoding_rsp(rsp_buf);
    }
    // Make the slice large enough to write into
    rsp_buf.set_len(rsp_buf.capacity());
    match sprocket.handle(req_buf.as_slice(), rsp_buf.as_mut_slice()) {
        Ok(size) => {
            rsp_buf.set_len(size);
            ringbuf_entry!(UartLog::ValidReq);
        }
        Err(_) => bad_encoding_rsp(rsp_buf),
    }
}

// Serialize an Error response for a poorly encoded request
fn bad_encoding_rsp(rsp_buf: &mut SliceVec<u8>) {
    // Make the slice large enough to write into
    rsp_buf.set_len(rsp_buf.capacity());
    let rsp = RotResponse::V1 {
        id: 0,
        result: RotResult::Err(RotError::BadEncoding),
    };
    let size = serialize(&mut rsp_buf.as_mut_slice(), &rsp).unwrap();

    // Properly size the slice for reading
    rsp_buf.set_len(size);
    ringbuf_entry!(UartLog::JunkReq);
}

// Decode a corncobs frame
fn decode_frame(req_buf: &mut SliceVec<u8>) -> Result<(), RotError> {
    let size = corncobs::decode_in_place(req_buf.as_mut_slice())
        .map_err(|_| RotError::BadEncoding)?;
    req_buf.set_len(size);
    Ok(())
}

// push as much of `data` as we can into `uart`'s TX FIFO, returning the number
// of bytes enqueued
fn tx_until_fifo_full(uart: &Usart, data: &[u8]) -> usize {
    for (i, &byte) in data.iter().enumerate() {
        if !try_tx_push(uart, byte) {
            return i;
        }
    }
    data.len()
}

// wrapper around `usart.try_tx_push()` that registers the result in our
// ringbuf
fn try_tx_push(usart: &Usart, val: u8) -> bool {
    let ret = usart.try_tx_push(val);
    if ret {
        ringbuf_entry!(UartLog::Tx(val));
    } else {
        ringbuf_entry!(UartLog::TxFull);
    }
    ret
}

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device() -> Usart {
    use drv_usart::device;
    use drv_usart::drv_stm32xx_sys_api::*;

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
        } else if #[cfg(feature = "usart3")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::D.pin(8).and_pin(9), Alternate::AF7),
            ];
            usart = unsafe { &*device::USART3::ptr() };
            peripheral = Peripheral::Usart3;
            pins = PINS;
        } else if #[cfg(feature = "uart4")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::D.pin(0).and_pin(1), Alternate::AF8),
            ];
            usart = unsafe { &*device::UART4::ptr() };
            peripheral = Peripheral::Uart4;
            pins = PINS;
        } else if #[cfg(feature = "uart5")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::C.pin(12), Alternate::AF8),
                (Port::D.pin(2), Alternate::AF8),
            ];
            usart = unsafe { &*device::UART5::ptr() };
            peripheral = Peripheral::Uart5;
            pins = PINS;
        } else if #[cfg(feature = "usart6")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::C.pin(6).and_pin(7), Alternate::AF7),
            ];
            usart = unsafe { &*device::USART6::ptr() };
            peripheral = Peripheral::Usart6;
            pins = PINS;
        } else if #[cfg(feature = "uart7")] {
            const PINS: &[(PinSet, Alternate)] = &[
                (Port::E.pin(7).and_pin(8), Alternate::AF7),
            ];
            usart = unsafe { &*device::UART7::ptr() };
            peripheral = Peripheral::Uart7;
            pins = PINS;
        } else {
            compiler_error!("no usartX/uartX feature specified");
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
