// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! See drv/sprot-api/README.md
//! Messages are received from the Service Processor (SP) over a SPI interface.
//!
//! Only one request from the SP or one reply from the RoT will be handled
//! inside the io loop. This pattern does allow for potential pipelining of up
//! to 2 requests from the SP, with no changes on the RoT. Currently, however,
//! in the happy path, the SP will only send one request, wait for ROT_IRQ,
//! to be asserted by the RoT, and then clock in a response while clocking
//! out zeros. In this common case, the RoT will be clocking out zeros while
//! clocking in the request from the SP.
//!
//! See drv/sprot-api for message layout.
//!
//! If the payload length exceeds the maximum size or not all bytes are received
//! before CSn is de-asserted, the message is malformed and an ErrorRsp message
//! will be sent to the SP in the next message exchange.
//!
//! Messages from the SP are not processed until the SPI chip-select signal
//! is deasserted.
//!
//! ROT_IRQ is intended to be an edge triggered interrupt on the SP.
//! TODO: ROT_IRQ is currently sampled by the SP.
//! ROT_IRQ is de-asserted only after CSn is deasserted.
//!
//! TODO: SP RESET needs to be monitored, otherwise, any
//! forced looping here could be a denial of service attack against
//! observation of SP resetting. SP resetting without invalidating
//! security related state means a compromised SP could operate using
//! the trust gained in the previous session.
//! Upper layers may mitigate that, but check on it.

#![no_std]
#![no_main]

use drv_lpc55_gpio_api::{Direction, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_sprot_api::{RotIoStats, MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE};
use lpc55_pac as device;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{
    sys_irq_control, sys_recv_closed, task_slot, TaskId, UnwrapLite,
};

mod handler;

use handler::Handler;

#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Trace {
    None,
    Dump(u32),
    Req { protocol: u8, body_type: u8 },
    ReceivedBytes(usize),
    SentBytes(usize),
    Flush,
    FlowError,
    StatusReq,
    ReplyLen(usize),
    Underrun,
}
ringbuf!(Trace, 16, Trace::None);

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

/// Setup spi and its associated GPIO pins
fn configure_spi() -> Io {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    let gpio_driver = GPIO.get_task_id();
    setup_pins(gpio_driver).unwrap_lite();
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

    // Configure ROT_IRQ
    // Ensure that ROT_IRQ is not asserted
    gpio.set_dir(ROT_IRQ, Direction::Output);
    gpio.set_val(ROT_IRQ, Value::One);

    // We have two blocks to worry about: the FLEXCOMM for switching
    // between modes and the actual SPI block. These are technically
    // part of the same block for the purposes of a register block
    // in app.toml but separate for the purposes of writing here

    let flexcomm = unsafe { &*device::FLEXCOMM8::ptr() };

    let registers = unsafe { &*device::SPI8::ptr() };

    let mut spi = spi_core::Spi::from(registers);

    // This should correspond to SPI mode 0
    spi.initialize(
        device::spi0::cfg::MASTER_A::SLAVE_MODE,
        device::spi0::cfg::LSBF_A::STANDARD, // MSB First
        device::spi0::cfg::CPHA_A::CHANGE,
        device::spi0::cfg::CPOL_A::LOW,
        spi_core::TxLvl::Tx7Items,
        spi_core::RxLvl::Rx1Item,
    );
    // Set SPI mode for Flexcomm
    flexcomm.pselid.write(|w| w.persel().spi());

    // Drain and configure FIFOs
    spi.enable();

    // We only want interrupts on CSn assert
    // Once we see that interrupt we enter polling mode
    // and check the registers manually.
    spi.ssa_enable();

    // Probably not necessary, drain Rx and Tx after config.
    spi.drain();

    // Disable the interrupts triggered by the `self.spi.drain_tx()`, which
    // unneccessarily causes spurious interrupts. We really only need to to
    // respond to CSn asserted interrupts, because after that we always enter a
    // tight loop.
    spi.disable_tx();
    spi.disable_rx();

    Io {
        spi,
        gpio,
        stats: RotIoStats::default(),
    }
}

// Container for spi and gpio
struct Io {
    spi: crate::spi_core::Spi,
    gpio: drv_lpc55_gpio_api::Pins,
    stats: RotIoStats,
}

enum IoError {
    Flush,
    Flow,
}

#[export_name = "main"]
fn main() -> ! {
    let mut io = configure_spi();

    let mut rx_buf = [0u8; MAX_REQUEST_SIZE];
    let mut tx_buf = [0u8; MAX_RESPONSE_SIZE];

    let mut handler = Handler::new();

    // Prime our write fifo, so we clock out zero bytes on the next receive
    io.spi.drain_tx();
    while io.spi.can_tx() {
        io.spi.send_u8(0);
    }

    loop {
        let rsp_len = match io.wait_for_request(&mut rx_buf) {
            Ok(rx_len) => {
                handler.handle(&rx_buf[..rx_len], &mut tx_buf, &mut io.stats)
            }
            Err(IoError::Flush) => {
                ringbuf_entry!(Trace::Flush);
                continue;
            }
            Err(IoError::Flow) => {
                ringbuf_entry!(Trace::FlowError);
                handler.flow_error(&mut tx_buf)
            }
        };

        io.reply(&tx_buf[..rsp_len]);
    }
}

impl Io {
    pub fn wait_for_request(
        &mut self,
        rx_buf: &mut [u8],
    ) -> Result<usize, IoError> {
        loop {
            sys_irq_control(notifications::SPI_IRQ_MASK, true);
            sys_recv_closed(
                &mut [],
                notifications::SPI_IRQ_MASK,
                TaskId::KERNEL,
            )
            .unwrap_lite();

            // Is CSn asserted by the SP?
            let intstat = self.spi.intstat();
            if intstat.ssa().bit() {
                self.spi.ssa_clear();
                break;
            }
        }

        // Go into a tight loop receiving as many bytes as we can until we see
        // CSn de-asserted.
        let mut bytes_received = 0;
        let mut rx = rx_buf.iter_mut();
        while !self.spi.ssd() {
            while self.spi.has_byte() {
                bytes_received += 1;
                let read = self.spi.read_u8();
                rx.next().map(|b| *b = read);
            }
        }

        self.spi.ssd_clear();

        // There may be bytes left in the rx fifo after CSn is de-asserted
        while self.spi.has_byte() {
            bytes_received += 1;
            let read = self.spi.read_u8();
            rx.next().map(|b| *b = read);
        }

        self.check_for_overrun()?;

        if bytes_received == 0 {
            // This was a CSn pulse
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            return Err(IoError::Flush);
        }

        ringbuf_entry!(Trace::ReceivedBytes(bytes_received));

        Ok(bytes_received)
    }

    fn reply(&mut self, tx_buf: &[u8]) {
        ringbuf_entry!(Trace::ReplyLen(tx_buf.len()));

        let mut tx = tx_buf.iter();

        // Fill in the fifo before we assert ROT_IRQ
        self.spi.drain_tx();
        while self.spi.can_tx() {
            let b = tx.next().copied().unwrap_or(0);
            self.spi.send_u8(b);
        }

        let mut rot_irq_asserted = false;
        loop {
            sys_irq_control(notifications::SPI_IRQ_MASK, true);

            if !rot_irq_asserted {
                rot_irq_asserted = true;
                self.assert_rot_irq();
            }

            sys_recv_closed(
                &mut [],
                notifications::SPI_IRQ_MASK,
                TaskId::KERNEL,
            )
            .unwrap_lite();

            // Is CSn asserted by the SP?
            let intstat = self.spi.intstat();
            if intstat.ssa().bit() {
                self.spi.ssa_clear();
                break;
            }
        }

        let mut bytes_sent = 0;
        while !self.spi.ssd() {
            while self.spi.can_tx() {
                bytes_sent += 1;
                let b = tx.next().copied().unwrap_or(0);
                self.spi.send_u8(b);
            }
        }

        self.spi.ssd_clear();

        // We clocked out at least an existing byte in the fifo, as we fill it on entry to
        // this function.
        if (bytes_sent == 0) || self.spi.can_tx() {
            // This was a CSn pulse
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
        } else {
            self.check_for_underrun();
        }

        // Prime our write fifo, so we clock out zero bytes on the next receive
        // We also empty our read fifo, since we don't bother reading bytes while writing.
        self.spi.drain();
        while self.spi.can_tx() {
            self.spi.send_u8(0);
        }

        ringbuf_entry!(Trace::SentBytes(bytes_sent));

        self.deassert_rot_irq();
    }

    fn check_for_overrun(&mut self) -> Result<(), IoError> {
        let fifostat = self.spi.fifostat();
        if fifostat.rxerr().bit() {
            self.spi.rxerr_clear();
            self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
            Err(IoError::Flow)
        } else {
            Ok(())
        }
    }

    // We don't actually want to return an error here.
    // The SP will detect an underrun via a CRC error
    fn check_for_underrun(&mut self) {
        let fifostat = self.spi.fifostat();

        if fifostat.txerr().bit() {
            // We don't do anything with tx errors other than record them
            // The SP will see a checksum error if this is a reply, or the
            // underrun happened after the number of reply bytes and it
            // doesn't matter.
            self.spi.txerr_clear();
            self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
            ringbuf_entry!(Trace::Underrun);
        }
    }

    fn assert_rot_irq(&self) {
        self.gpio.set_val(ROT_IRQ, Value::Zero);
    }

    fn deassert_rot_irq(&self) {
        self.gpio.set_val(ROT_IRQ, Value::One);
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi);
    syscon.leave_reset(Peripheral::HsLspi);
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
