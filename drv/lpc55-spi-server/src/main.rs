// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!

#![no_std]
#![no_main]

use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use ringbuf::*;
use userlib::{sys_irq_control, sys_recv_notification, task_slot};

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Irq,
    Tx(u8),
    Rx(u8),
}

ringbuf!(Trace, 64, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    let gpio_driver = GPIO.get_task_id();

    setup_pins(gpio_driver).unwrap_lite();

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual SPI block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM8::ptr() };

    let registers = unsafe { &*device::SPI8::ptr() };

    let mut spi = spi_core::Spi::from(registers);

    // Set SPI mode for Flexcomm
    flexcomm.pselid.write(|w| w.persel().spi());

    // This should correspond to SPI mode 0
    spi.initialize(
        device::spi0::cfg::MASTER_A::SLAVE_MODE,
        device::spi0::cfg::LSBF_A::STANDARD, // MSB First
        device::spi0::cfg::CPHA_A::CHANGE,
        device::spi0::cfg::CPOL_A::LOW,
        spi_core::TxLvl::Tx7Items,
        spi_core::RxLvl::Rx1Item,
    );

    spi.enable();

    let mut a_bytes: [u8; 8] = [0xaa; 8];
    let mut b_bytes: [u8; 8] = [0; 8];

    sys_irq_control(notifications::SPI_IRQ_MASK, true);

    let mut tx = &mut a_bytes;
    let mut rx = &mut b_bytes;

    let mut tx_cnt = 0;
    let mut rx_cnt = 0;

    spi.drain();
    spi.enable_rx();
    spi.enable_tx();

    let mut tx_done = false;
    let mut rx_done = false;

    loop {
        sys_recv_notification(notifications::SPI_IRQ_MASK);

        ringbuf_entry!(Trace::Irq);

        loop {
            let mut again = false;

            if spi.can_tx() && !tx_done {
                let b = tx[tx_cnt];
                ringbuf_entry!(Trace::Tx(b));
                spi.send_u8(b);
                tx_cnt += 1;
                if tx_cnt == tx.len() {
                    tx_done = true;
                }
                again = true;
            }

            if spi.has_entry() && !rx_done {
                let b = spi.read_u8();
                ringbuf_entry!(Trace::Rx(b));
                rx[rx_cnt] = b;
                rx_cnt += 1;
                if rx_cnt == rx.len() {
                    rx_done = true;
                }

                again = true;
            }

            if !again {
                break;
            }
        }

        if tx_done && rx_done {
            core::mem::swap(&mut rx, &mut tx);
            rx_done = false;
            tx_done = false;
            tx_cnt = 0;
            rx_cnt = 0;
        }

        sys_irq_control(notifications::SPI_IRQ_MASK, true);
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi);
    syscon.leave_reset(Peripheral::HsLspi);

    syscon.enable_clock(Peripheral::Fc3);
    syscon.leave_reset(Peripheral::Fc3);
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
