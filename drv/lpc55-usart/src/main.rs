// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 U(S)ART.
//!
//! This driver is currently configured to run at 9600. We could potentially
//! run faster but 9600 works so nicely with the clocks...
//!
//! # IPC protocol
//!
//! ## `write` (1)
//!
//! Sends the contents of lease #0. Returns when completed.

#![no_std]
#![no_main]

use core::ops::Deref;
use drv_lpc55_syscon_api::*;
use lib_lpc55_usart::{Usart, Write};
use lpc55_pac as device;
use userlib::*;
use zerocopy::IntoBytes;

task_slot!(SYSCON, syscon_driver);

const OP_WRITE: u32 = 1;

task_slot!(GPIO, gpio_driver);

#[repr(u32)]
enum ResponseCode {
    Success = 0,
    BadOp = 1,
    BadArg = 2,
    Busy = 3,
}

struct Transmit {
    task: TaskId,
    len: usize,
    pos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm();

    let gpio_driver = GPIO.get_task_id();

    setup_pins(gpio_driver).unwrap_lite();

    let peripherals = device::Peripherals::take().unwrap_lite();
    let usart = peripherals.USART0;
    let flexcomm = peripherals.FLEXCOMM0;

    // Set flexcom to be a USART
    // drv-lpc55-syscon sets flexcomm0 to use the 12Mhz clock
    flexcomm.pselid.write(|w| w.persel().usart());

    let mut usart = Usart::from(usart.deref());

    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(notifications::USART_IRQ_MASK, true);

    // Field messages.
    let mut tx: Option<Transmit> = None;

    loop {
        let msginfo = sys_recv_open(&mut [], notifications::USART_IRQ_MASK);
        if msginfo.sender == TaskId::KERNEL {
            if msginfo.operation & 1 != 0 {
                // Handling an interrupt. To allow for spurious interrupts,
                // check the individual conditions we care about, and
                // unconditionally re-enable the IRQ at the end of the handler.
                if let Some(txs) = tx.as_mut() {
                    // Transmit in progress, check to see if TX is empty.
                    if usart.is_tx_idle() {
                        // TX register empty. Time to send something.
                        if step_transmit(&mut usart, txs) {
                            tx = None;
                            // This is a write to clear register
                            usart.clear_tx_idle_interrupt();
                        }
                    }
                }

                sys_irq_control(notifications::USART_IRQ_MASK, true);
            }
        } else {
            match msginfo.operation {
                OP_WRITE => {
                    // Deny incoming writes if we're already running one.
                    if tx.is_some() {
                        sys_reply(
                            msginfo.sender,
                            ResponseCode::Busy as u32,
                            &[],
                        );
                        continue;
                    }

                    // Check the lease count and characteristics.
                    if msginfo.lease_count != 1 {
                        sys_reply(
                            msginfo.sender,
                            ResponseCode::BadArg as u32,
                            &[],
                        );
                        continue;
                    }

                    let len = match sys_borrow_info(msginfo.sender, 0) {
                        None => {
                            sys_reply(
                                msginfo.sender,
                                ResponseCode::BadArg as u32,
                                &[],
                            );
                            continue;
                        }
                        Some(info)
                            if !info
                                .attributes
                                .contains(LeaseAttributes::READ) =>
                        {
                            sys_reply(
                                msginfo.sender,
                                ResponseCode::BadArg as u32,
                                &[],
                            );
                            continue;
                        }
                        Some(info) => info.len,
                    };

                    // Okay! Begin a transfer!
                    tx = Some(Transmit {
                        task: msginfo.sender,
                        pos: 0,
                        len,
                    });

                    usart.set_tx_idle_interrupt();

                    // We'll do the rest as interrupts arrive.
                }
                _ => sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]),
            }
        }
    }
}

fn turn_on_flexcomm() {
    let syscon = Syscon::from(SYSCON.get_task_id());

    syscon.enable_clock(Peripheral::Fc0);
    syscon.leave_reset(Peripheral::Fc0);
}

fn step_transmit(usart: &mut Usart<'_>, txs: &mut Transmit) -> bool {
    let mut byte = 0u8;
    let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_mut_bytes());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        true
    } else {
        // Stuff byte into transmitter.
        match usart.write(byte) {
            Ok(_) => {
                txs.pos += 1;
                if txs.pos == txs.len {
                    sys_reply(txs.task, ResponseCode::Success as u32, &[]);
                    true
                } else {
                    false
                }
            }
            Err(nb::Error::WouldBlock) => false,
            Err(nb::Error::Other(e)) => {
                panic!("write to Usart failed: {:?}", e)
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
