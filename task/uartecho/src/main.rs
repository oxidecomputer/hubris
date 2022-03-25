// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]
#![feature(asm)]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as usart;

use ringbuf::*;
use tinyvec::ArrayVec;
use usart::RxError;
use usart::TxBuf;
use userlib::*;

task_slot!(SYS, sys);

#[derive(Debug, Clone, Copy, PartialEq)]
enum UartLog {
    Tx(u8),
    TxOverrun(u8),
    Rx(u8),
    RxOverrun,
}

ringbuf!(UartLog, 32, UartLog::Rx(0));

/// Notification mask for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1;

/// Size in bytes of our in-memory TX/RX buffers.
const BUF_LEN: usize = 32;

type Usart = usart::Usart<BUF_LEN, BUF_LEN>;

#[export_name = "main"]
fn main() -> ! {
    let device = configure_uart_device();
    let mut uart = Usart::new(device, USART_IRQ);
    let mut line_buf = ArrayVec::<[u8; BUF_LEN]>::new();

    loop {
        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let _ = sys_recv_closed(&mut [], USART_IRQ, TaskId::KERNEL);

        // step uart, transmitting the next byte we have to give (if possible
        // and we have one)
        uart.handle_interrupt();

        let (mut tx, rx) = uart.buffers();
        let rx_buf = match rx.drain() {
            Ok(rx) => rx,
            Err((rx, RxError::Overrun)) => {
                ringbuf_entry!(UartLog::RxOverrun);
                rx
            }
        };

        for &rx in rx_buf.as_slice() {
            ringbuf_entry!(UartLog::Rx(rx));

            // minicom default settings only ever sends `\r` as line
            // endings, so we only check for that, but we still send it "\r\n"
            // so the output looks correct
            if rx == b'\r' {
                // try send back newline, previous line if we have room
                try_push_ringbuf_log(&mut tx, b'\r');
                try_push_ringbuf_log(&mut tx, b'\n');
                for b in line_buf.drain(..line_buf.len()) {
                    try_push_ringbuf_log(&mut tx, b);
                }

                // always send back a newline, even if we have to drop some
                // bytes to do so
                tx.truncate(BUF_LEN - 2);
                try_push_ringbuf_log(&mut tx, b'\r');
                try_push_ringbuf_log(&mut tx, b'\n');
            } else {
                // not a newline; append to both tx_buf (to immediately echo)
                // and line_buf (to send back the whole line once we see a
                // newline)
                try_push_ringbuf_log(&mut tx, rx);
                let _ = line_buf.try_push(rx);
            }
        }

        // Uncomment this to artifically slow down the task to make it easy to
        // see RxOverrun errors
        //hl::sleep_for(200);
    }
}

// wrapper around `tx.try_push()` that registers the result in our ringbuf
fn try_push_ringbuf_log<const N: usize>(tx: &mut TxBuf<'_, N>, val: u8) {
    match tx.try_push(val) {
        None => ringbuf_entry!(UartLog::Tx(val)),
        Some(_) => ringbuf_entry!(UartLog::TxOverrun(val)),
    }
}

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device() -> usart::stm32h7::Device {
    use usart::stm32h7::device;
    use usart::stm32h7::drv_stm32xx_sys_api::*;
    use usart::stm32h7::Device;

    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 100_000_000;

    const BAUD_RATE: u32 = 115_600;

    // From thin air, pluck a pointer to the USART register block.
    //
    // Safety: this is needlessly unsafe in the API. The USART is essentially a
    // static, and we access it through a & reference so aliasing is not a
    // concern. Were it literally a static, we could just reference it.
    let usart = unsafe { &*device::USART3::ptr() };

    Device::turn_on(
        &Sys::from(SYS.get_task_id()),
        usart,
        Peripheral::Usart3,
        Port::D.pin(8).and_pin(9),
        Alternate::AF7,
        CLOCK_HZ,
        BAUD_RATE,
    )
}
