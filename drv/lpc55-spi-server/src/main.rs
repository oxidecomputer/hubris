// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! Mostly for demonstration purposes, write is verified read is not
//!
//! # IPC protocol
//!
//! ## `read` (1)
//!
//! Reads the buffer into lease #0. Returns when completed
//!
//!
//! ## `write` (2)
//!
//! Sends the contents of lease #0. Returns when completed.
//!
//! ## `exchange` (3)
//!
//! Sends the contents of lease #0 and writes received data into lease #1

#![no_std]
#![no_main]

use drv_lpc55_gpio_api::*;
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use lpc55_pac as device;
use ringbuf::*;
use userlib::*;

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    IRQ,
    Tx(u8),
    Rx(u8),
    None,
}

ringbuf!(Trace, 64, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    muck_with_gpios(&syscon);

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

    sys_irq_control(1, true);

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
        if sys_recv_closed(&mut [], 1, TaskId::KERNEL).is_err() {
            panic!()
        }

        ringbuf_entry!(Trace::IRQ);

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

            if spi.has_byte() && !rx_done {
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
            let tmp = rx;

            rx = tx;
            tx = tmp;
            rx_done = false;
            tx_done = false;
            tx_cnt = 0;
            rx_cnt = 0;
        }

        sys_irq_control(1, true);
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

fn muck_with_gpios(syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Iocon);
    syscon.leave_reset(Peripheral::Iocon);

    let gpio_driver = GPIO.get_task_id();
    let iocon = Pins::from(gpio_driver);

    // This is quite the array!
    // All of these need to be in digital mode. The NXP C driver
    // also sets the pull-up resistor

    let pin_settings = [
        // HS_SPI_MOSI = P0_26 = FUN9
        (
            Pin::PIO0_26,
            AltFn::Alt9,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_MISO = P1_3 = FUN6
        (
            Pin::PIO1_3,
            AltFn::Alt6,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_SCK = P1_2 = FUN6
        (
            Pin::PIO1_2,
            AltFn::Alt6,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
        // HS_SPI_SSEL1 = P1_1 = FUN5
        // Note that SSEL0, SSEL2 and SSEL3 are not used in the current design
        (
            Pin::PIO1_1,
            AltFn::Alt5,
            Mode::PullUp,
            Slew::Standard,
            Invert::Disable,
            Digimode::Digital,
            Opendrain::Normal,
        ),
    ];

    for (pin, alt, mode, slew, invert, digi, od) in
        core::array::IntoIter::new(pin_settings)
    {
        iocon
            .iocon_configure(pin, alt, mode, slew, invert, digi, od)
            .unwrap();
    }
}
