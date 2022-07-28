// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_usart::Usart;
use ringbuf::*;
use tinyvec::ArrayVec;
use userlib::*;

task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum UartLog {
    Tx(u8),
    TxFull,
    Rx(u8),
    RxOverrun,
}

ringbuf!(UartLog, 64, UartLog::Rx(0));

/// Notification mask for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1;

/// Size in bytes of our in-memory buffer to store a line to echo back; lines
/// longer than this will be truncated to this many bytes.
const BUF_LEN: usize = 32;

enum NeedToTx {
    FlushLineStart(&'static [u8]),
    FlushLine,
    FlushLineEnd(&'static [u8]),
    EchoPreviousByte(u8),
}

#[export_name = "main"]
fn main() -> ! {
    let uart = configure_uart_device();
    let mut line_buf = ArrayVec::<[u8; BUF_LEN]>::new();
    let mut need_to_tx = None;

    sys_irq_control(USART_IRQ, true);

    loop {
        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let _ = sys_recv_closed(&mut [], USART_IRQ, TaskId::KERNEL);

        // Walk through our tx state machine to handle echoing lines back; note
        // that many of these cases intentionally break after refilling
        // `need_to_tx` if we fill the TX fifo.
        while let Some(tx_state) = need_to_tx.take() {
            match tx_state {
                NeedToTx::FlushLineStart(mut crnl) => {
                    crnl = &crnl[tx_until_fifo_full(&uart, crnl)..];
                    if crnl.is_empty() {
                        need_to_tx = Some(NeedToTx::FlushLine);
                    } else {
                        need_to_tx = Some(NeedToTx::FlushLineStart(crnl));
                        break;
                    }
                }
                NeedToTx::FlushLine => {
                    let n = tx_until_fifo_full(&uart, &line_buf);
                    line_buf.drain(..n);
                    if line_buf.is_empty() {
                        need_to_tx = Some(NeedToTx::FlushLineEnd(b"\r\n"));
                    } else {
                        need_to_tx = Some(NeedToTx::FlushLine);
                        break;
                    }
                }
                NeedToTx::FlushLineEnd(mut crnl) => {
                    crnl = &crnl[tx_until_fifo_full(&uart, crnl)..];
                    if !crnl.is_empty() {
                        need_to_tx = Some(NeedToTx::FlushLineStart(crnl));
                    }
                    break;
                }

                // this state isn't for line echo; this is the case where we
                // pulled a byte out of the RX fifo but couldn't immediately put
                // it back into the TX fifo
                NeedToTx::EchoPreviousByte(byte) => {
                    if !try_tx_push(&uart, byte) {
                        need_to_tx = Some(NeedToTx::EchoPreviousByte(byte));
                    }
                    break;
                }
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

            // minicom default settings only ever sends `\r` as line
            // endings, so we only check for that to decide when to echo a line
            if byte == b'\r' {
                uart.enable_tx_fifo_empty_interrupt();
                need_to_tx = Some(NeedToTx::FlushLineStart(b"\r\n"));
                break;
            }

            // not a line end. stash it in `line_buf` if there's room...
            let _ = line_buf.try_push(byte);

            // ...and echo it back
            if !try_tx_push(&uart, byte) {
                uart.enable_tx_fifo_empty_interrupt();
                need_to_tx = Some(NeedToTx::EchoPreviousByte(byte));
                break;
            }
        }

        // rennable USART interrupts
        sys_irq_control(USART_IRQ, true);

        // Uncomment this to artifically slow down the task to make it easier to
        // see RxOverrun errors
        //hl::sleep_for(200);
    }
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
    )
}
