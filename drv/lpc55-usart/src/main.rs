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

use drv_lpc55_gpio_api::*;
use drv_lpc55_syscon_api::*;
use lpc55_pac as device;
use userlib::*;
use zerocopy::AsBytes;

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

    muck_with_gpios();

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual USART. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM0::ptr() };

    let usart = unsafe { &*device::USART0::ptr() };

    // Set USART mode
    flexcomm.pselid.write(|w| w.persel().usart());

    usart.fifocfg.modify(|_, w| w.enabletx().enabled());

    // We actually get interrupts from the FIFO
    // Trigger when the FIFO is empty for now
    usart
        .fifotrig
        .modify(|_, w| unsafe { w.txlvl().bits(0).txlvlena().enabled() });

    // This puts us at 9600 baud because it divides nicely with the
    // 12mhz clock
    usart.brg.write(|w| unsafe { w.brgval().bits(0x7c) });
    usart.osr.write(|w| unsafe { w.osrval().bits(0x9) });

    // 8N1 configuration
    usart.cfg.write(|w| unsafe {
        w.paritysel()
            .bits(0)
            .stoplen()
            .bit(false)
            .datalen()
            .bits(1)
            .loop_()
            .normal()
            .syncen()
            .asynchronous_mode()
            .clkpol()
            .falling_edge()
            .enable()
            .enabled()
    });

    // USART side yet, so this won't trigger notifications yet.
    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut tx: Option<Transmit> = None;

    loop {
        let msginfo = sys_recv_open(&mut [], mask);
        if msginfo.sender == TaskId::KERNEL {
            if msginfo.operation & 1 != 0 {
                // Handling an interrupt. To allow for spurious interrupts,
                // check the individual conditions we care about, and
                // unconditionally re-enable the IRQ at the end of the handler.
                if let Some(txs) = tx.as_mut() {
                    // Transmit in progress, check to see if TX is empty.
                    if usart.stat.read().txidle().bit() {
                        // TX register empty. Time to send something.
                        if step_transmit(&usart, txs) {
                            tx = None;
                            // This is a write to clear register
                            usart.intenclr.write(|w| w.txidleclr().set_bit());
                        }
                    }
                }

                sys_irq_control(1, true);
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

                    let (rc, atts, len) = sys_borrow_info(msginfo.sender, 0);
                    if rc != 0 || atts & 1 == 0 {
                        sys_reply(
                            msginfo.sender,
                            ResponseCode::BadArg as u32,
                            &[],
                        );
                        continue;
                    }

                    // Okay! Begin a transfer!
                    tx = Some(Transmit {
                        task: msginfo.sender,
                        pos: 0,
                        len,
                    });

                    usart.intenset.modify(|_, w| w.txidleen().set_bit());

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

fn muck_with_gpios() {
    let gpio_driver = GPIO.get_task_id();
    let iocon = Gpio::from(gpio_driver);

    // Our GPIOs are P0_29 and P0_30 and need to be set to AF1

    iocon
        .iocon_configure(
            Pin::PIO0_29,
            AltFn::Alt1,
            Mode::NoPull,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        )
        .unwrap();

    iocon
        .iocon_configure(
            Pin::PIO0_30,
            AltFn::Alt1,
            Mode::NoPull,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        )
        .unwrap();
}

fn step_transmit(
    usart: &device::usart0::RegisterBlock,
    txs: &mut Transmit,
) -> bool {
    let mut byte = 0u8;
    let (rc, len) = sys_borrow_read(txs.task, 0, txs.pos, byte.as_bytes_mut());
    if rc != 0 || len != 1 {
        sys_reply(txs.task, ResponseCode::BadArg as u32, &[]);
        true
    } else {
        // Stuff byte into transmitter.
        //
        // This is marked as unsafe for reasons I don't quite understand?
        unsafe {
            usart.fifowr.write(|w| w.bits(byte as u32));
        }

        txs.pos += 1;
        if txs.pos == txs.len {
            sys_reply(txs.task, ResponseCode::Success as u32, &[]);
            true
        } else {
            false
        }
    }
}
