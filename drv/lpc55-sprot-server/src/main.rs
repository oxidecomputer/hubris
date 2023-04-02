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
use drv_sprot_api::{
    MsgHeader, Protocol, RotIoStats, RxMsg, SprotError, TxMsg, BUF_SIZE,
    HEADER_SIZE,
};
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
    ErrWithHeader(SprotError, [u8; HEADER_SIZE]),
    ErrWithTypedHeader(SprotError, MsgHeader),
    Dump(u32),
    Fifostat(u32),
    CsnDeasserted(bool),
    CsnDeassertedBreak,
    FlowError(Option<Protocol>, usize),
    Stat(u32),
    Replying(bool),
    ReadRemainingFromFifo,
}
ringbuf!(Trace, 128, Trace::None);

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
    // It should never happen but, initialization should be able to deal
    // with CSn being asserted at init time.

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

    // We only want interrupts on CSn assert
    // Once we see that interrupt we enter polling mode
    // and check the registers manually.
    spi.ssa_enable();
    spi.ssd_disable();
    spi.mstidle_disable();

    // Disable the interrupts triggered by the `self.spi.drain_tx()`, which
    // unneccessarily causes spurious interrupts. We really only need to to
    // respond to CSn asserted interrupts, because after that we always enter a
    // tight loop.
    spi.disable_tx();
    spi.disable_rx();

    // Drain and configure FIFOs
    spi.enable();

    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

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

    let mut rx_buf = [0u8; BUF_SIZE];
    let mut tx_buf = [0u8; BUF_SIZE];

    let mut handler = Handler::new();
    let mut signal_reply = false;

    loop {
        // Every time through the io loop we receive a fresh request
        let mut rx_msg = RxMsg::new(&mut rx_buf[..]);

        let reply = match io.next(signal_reply, &tx_buf[..], &mut rx_msg) {
            Ok(()) => {
                // Put a busy byte in the tx_fifo before we handle the result,
                // which may take some time.
                io.mark_busy();
                // Clear the buffer in preparation for a reply
                let tx_msg = TxMsg::new(&mut tx_buf[..]);
                handler.handle(rx_msg, tx_msg, &mut io.stats)
            }
            Err(IoError::Flush) => None,
            Err(IoError::Flow) => {
                io.mark_busy();
                let tx_msg = TxMsg::new(&mut tx_buf[..]);
                Some(handler.flow_error(tx_msg))
            }
        };

        if reply.is_none() {
            // There's no reply to send, so we should clear the Tx buffer.
            tx_buf.fill(0);
            signal_reply = false;
        } else {
            signal_reply = true;
        }
    }
}

impl Io {
    /// Put a Protocol::Busy value in FIFOWR so that SP/logic analyzer knows
    /// we're away.
    pub fn mark_busy(&mut self) {
        self.spi.drain_tx();
        self.spi.send_u8(Protocol::Busy as u8);
    }

    pub fn next<'a>(
        &mut self,
        signal_reply: bool,
        tx_buf: &[u8],
        rx_msg: &mut RxMsg<'a>,
    ) -> Result<(), IoError> {
        let mut tx_iter = tx_buf.iter();
        self.prime_tx_fifo(&mut tx_iter);

        sys_irq_control(notifications::SPI_IRQ_MASK, true);

        if signal_reply {
            self.assert_rot_irq();
        }

        sys_recv_closed(&mut [], notifications::SPI_IRQ_MASK, TaskId::KERNEL)
            .unwrap_lite();

        let result = self.tight_loop(&mut tx_iter, rx_msg, signal_reply);

        // We delay clearing until after the tight loop returns
        let _intstat = self.spi.intstat();
        self.spi.ssa_clear();

        if signal_reply {
            self.deassert_rot_irq();
        }

        result
    }

    // Read data in a tight loop until we see CSn de-asserted
    //
    // XXX Denial of service by forever asserting CSn?
    // We could mitigate by imposing a time limit
    // and resetting the SP if it is exceeded.
    // But, the management plane is going to notice that
    // the RoT is not available. So, does it matter?
    fn tight_loop<'a>(
        &mut self,
        tx_iter: &mut dyn Iterator<Item = &u8>,
        rx_msg: &mut RxMsg<'a>,
        replying: bool,
    ) -> Result<(), IoError> {
        let mut too_many_bytes_received = false;

        // If we read fifostat twice without having to read or write any data
        // we may be done.

        while !self.spi.ssd() {
            let fifostat = self.spi.fifostat();
            for _ in fifostat.txlvl().bits()..8u8 {
                if let Some(b) = tx_iter.next().copied() {
                    self.spi.send_u8(b);
                } else {
                    // Just clock out zeros and prevent an unnecessary underrun
                    self.spi.send_u8(0);
                }
            }
            for _ in 0..fifostat.rxlvl().bits() {
                let b = self.spi.read_u8();
                rx_msg.push(b);
            }
        }

        // Read any remaining data
        while self.spi.has_byte() {
            ringbuf_entry!(Trace::ReadRemainingFromFifo);
            let b = self.spi.read_u8();
            if rx_msg.push(b).is_err() {
                too_many_bytes_received = true;
            }
        }

        // Clear ssd
        self.spi.ssd_clear();

        // Keep track of some stats
        if too_many_bytes_received {
            self.stats.rx_protocol_error_too_many_bytes =
                self.stats.rx_protocol_error_too_many_bytes.wrapping_add(1);
        }

        // Let's check for any problems
        let fifostat = self.spi.fifostat();
        if fifostat.txerr().bit() {
            // We don't do anything with tx errors other than record them
            // The SP will see a checksum error if this is a reply, or the
            // underrun happened after the number of reply bytes and it
            // doesn't matter.
            self.spi.txerr_clear();
            self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
        }

        if fifostat.rxerr().bit() {
            // Rx errors are more important. They mean we're missing
            // data. We should report this to the SP. This can be used to
            // potentially throttle sends in the future.
            self.spi.rxerr_clear();
            self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
            // If we were just sending our response, and SP was
            // just sending zeros and we received the first byte
            // correctly and that first byte was zero, then
            // our Rx overrun is inconsequential and does not
            // need to be reported as a message.
            if rx_msg.len() > 0 && rx_msg.protocol() != Some(Protocol::Ignore) {
                // This error matters
                ringbuf_entry!(Trace::FlowError(
                    rx_msg.protocol(),
                    rx_msg.len()
                ));
                return Err(IoError::Flow);
            }
        }

        if rx_msg.is_empty() {
            // This was a CSn pulse
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            return Err(IoError::Flush);
        }

        Ok(())
    }

    // Prime the fifo with the first part of the response to prevent
    // underrun while waiting for an interrupt.
    fn prime_tx_fifo(&mut self, tx_iter: &mut dyn Iterator<Item = &u8>) {
        self.spi.drain_tx();
        while self.spi.can_tx() {
            if let Some(b) = tx_iter.next().copied() {
                self.spi.send_u8(b);
            } else {
                self.spi.send_u8(0);
            }
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
