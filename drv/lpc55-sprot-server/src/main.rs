// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::doc_overindented_list_items)]
//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! See drv/sprot-api/README.md
//! Messages are received from the Service Processor (SP) over a SPI interface.
//!
//! Only one request from the SP or one reply from the RoT will be handled
//! inside the io loop. The SP will only send one request, wait for ROT_IRQ to
//! be asserted by the RoT, assert CSn, then clock in a response while clocking
//! out zeros, and de-assert CSn before the RoT de-asserts ROT_IRQ. The RoT will
//! be clocking out zeros while clocking in the request from the SP.
//!
//! In ordered list form a full request/response interaction looks like this:
//!   1. SP sends request
//!     a. SP asserts CSn - detected at RoT via SSA interrupt
//!     b. SP clocks out request and clocks in zeroes from the RoT
//!     c. SP de-asserts CSn - detected at RoT via SSD bit in status register
//!   2. RoT sends a reply
//!     a. ROT asserts ROT_IRQ to signal it has a reply
//!     b. SP asserts CSn - detected at RoT via SSA interrupt
//!     c. SP clocks in a FIFO of data from the RoT while clocking out zeros
//!     d. SP decodes header, and if more data is to be read, clocks it in
//!        from the RoT while clocking out zeros
//!     e. SP de-asserts CSn - detected at RoT via SSD bit in status register
//!     f. RoT de-asserts ROT_IRQ to indicate it is done replying
//!     g. RoT goes back to waiting for the next request
//!
//! See drv/sprot-api for message layout.
//!
//! If the payload length exceeds the maximum size or not all bytes are received
//! before CSn is de-asserted, the message is malformed and an ErrorRsp message
//! will be sent to the SP in the next message exchange.
//!
//! ROT_IRQ is intended to be an edge triggered interrupt on the SP.
//! TODO: ROT_IRQ is currently sampled by the SP.
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
    RotIoStats, SprotProtocolError, REQUEST_BUF_SIZE, RESPONSE_BUF_SIZE,
    ROT_FIFO_SIZE,
};
use lpc55_pac as device;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{
    sys_irq_control, sys_recv_notification, task_slot, TaskId, UnwrapLite,
};

mod handler;

use handler::Handler;

#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Trace {
    None,
    ReceivedBytes(usize),
    Flush,
    FlowError,
    ReplyLen(usize),
    Underrun,
    Err(SprotProtocolError),
    Stats(RotIoStats),
    Desynchronized,

    #[cfg(feature = "sp-ctrl")]
    Dump(u32),
}
ringbuf!(Trace, 32, Trace::None);

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
    /// The RoT has received a CSn pulse from the SP
    Flush,

    /// The RoT has failed to receive bytes in a request due
    /// to an rxfifo overrun.
    Flow,

    /// The RoT has reason to believe that it is out of sync with the SP.
    ///
    /// In particular, the RoT may be trying to receive a request from the SP
    /// while the SP is trying to receive a response from the RoT, or the RoT
    /// may be trying to send a response to the SP while the SP is trying to
    /// send a request to the RoT. We also return this error if we started
    /// receiving a request in the middle.
    Desynchronized,
}

#[export_name = "main"]
fn main() -> ! {
    let mut io = configure_spi();
    let (rx_buf, tx_buf) = {
        use static_cell::ClaimOnceCell;
        static BUFS: ClaimOnceCell<(
            [u8; REQUEST_BUF_SIZE],
            [u8; RESPONSE_BUF_SIZE],
        )> =
            ClaimOnceCell::new(([0; REQUEST_BUF_SIZE], [0; RESPONSE_BUF_SIZE]));
        BUFS.claim()
    };

    let mut handler = Handler::new();

    // Prepare to receive our first request
    io.cleanup();
    io.prime_write_fifo_with_zeros();

    loop {
        let mut rsp_len = match io.wait_for_request(rx_buf) {
            Ok(rx_len) => {
                handler.handle(&rx_buf[..rx_len], tx_buf, &mut io.stats)
            }
            Err(IoError::Flush) => {
                // A flush indicates that the server should de-assert ROT_IRQ
                // as instructed by the SP. We do that and then proceed to wait
                // for the next request.
                ringbuf_entry!(Trace::Flush);
                let _ = io.cleanup_after_request();
                io.deassert_rot_irq();
                continue;
            }
            Err(IoError::Flow) => {
                ringbuf_entry!(Trace::FlowError);
                handler.flow_error(tx_buf)
            }
            Err(IoError::Desynchronized) => {
                ringbuf_entry!(Trace::Desynchronized);
                handler.desynchronized_error(tx_buf)
            }
        };

        if io.cleanup_after_request().is_err() {
            // Reply with a desync error, not whatever the response was
            rsp_len = handler.desynchronized_error(tx_buf);
        }

        ringbuf_entry!(Trace::Stats(io.stats));
        io.reply(&tx_buf[..rsp_len]);
    }
}

struct DesynchronizedError;
impl Io {
    fn wait_for_csn_asserted(&self) {
        loop {
            sys_irq_control(notifications::SPI_IRQ_MASK, true);

            sys_recv_notification(notifications::SPI_IRQ_MASK);

            // Is CSn asserted by the SP?
            let intstat = self.spi.intstat();
            if intstat.ssa().bit() {
                self.spi.ssa_clear();
                break;
            }
        }
    }

    // Wait for CSn to actually be de-asserted.
    //
    // This checks the actual gpio pin, not the SPI block saturating bit which
    // may be set from a prior deassertion.
    //
    // If CSn is still asserted at this point, then we are desynchronized and so
    // we return an error.
    //
    // See https://github.com/oxidecomputer/hubris/issues/1507 for why we do
    // this.
    fn wait_for_csn_deasserted(&mut self) -> Result<(), DesynchronizedError> {
        let mut result = Ok(());
        while self.gpio.read_val(CHIP_SELECT) != Value::One {
            ringbuf_entry!(Trace::Desynchronized);
            if result.is_ok() {
                self.stats.desynchronized =
                    self.stats.desynchronized.wrapping_add(1);
                result = Err(DesynchronizedError);
            }
        }
        result
    }

    // Wait for a request from the SP
    pub fn wait_for_request(
        &mut self,
        rx_buf: &mut [u8],
    ) -> Result<usize, IoError> {
        self.wait_for_csn_asserted();

        let mut bytes_received = 0;
        let mut rx = rx_buf.iter_mut();

        // Check for the SOT bit on first fifo read to see if we are
        // synchronized.
        let mut first_read = true;

        let mut read_fifo = || {
            while self.spi.has_entry() {
                bytes_received += 2;
                let (read, sot) = self.spi.read_u16_with_sot();
                if first_read {
                    first_read = false;
                    if !sot {
                        self.stats.desynchronized =
                            self.stats.desynchronized.wrapping_add(1);
                        return Err(IoError::Desynchronized);
                    }
                }
                let upper = (read >> 8) as u8;
                let lower = read as u8;
                if let Some(b) = rx.next() {
                    *b = upper;
                }
                if let Some(b) = rx.next() {
                    *b = lower;
                }
            }
            Ok(())
        };

        // Go into a tight loop receiving as many bytes as we can until we see
        // CSn de-asserted.
        //
        // This is the realtime part of the code
        while !self.spi.ssd() {
            read_fifo()?;
        }

        // There may be bytes left in the rx fifo after CSn is de-asserted
        while self.spi.has_entry() {
            read_fifo()?;
        }

        ringbuf_entry!(Trace::ReceivedBytes(bytes_received));

        self.check_for_rx_error()?;

        // Was this a CSn pulse?
        if bytes_received == 0 {
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            return Err(IoError::Flush);
        }

        Ok(bytes_received)
    }

    pub fn cleanup(&mut self) {
        // Drain our TX and RX fifos
        self.spi.drain();

        // Clear any errors
        self.spi.rxerr_clear();
        self.spi.txerr_clear();

        // Ensure our SSA/SSD bits are not set
        self.spi.ssa_clear();
        self.spi.ssd_clear();
    }

    pub fn cleanup_after_request(&mut self) -> Result<(), DesynchronizedError> {
        let result = self.wait_for_csn_deasserted();
        self.cleanup();
        result
    }

    // Reply to the SP
    //
    // At this point, our fifos have been drained as we prepare to send the next
    // reply. If the fifo's are not empty it means that a new request has begun
    // while we have started to reply. This will end up with either the SP or
    // RoT detecting an error and resynchronizing.
    pub fn reply(&mut self, tx_buf: &[u8]) {
        ringbuf_entry!(Trace::ReplyLen(tx_buf.len()));

        let mut idx = 0;
        let mut write_fifo = || {
            while self.spi.can_tx() {
                let entry = get_u16(idx, tx_buf);
                self.spi.send_u16(entry);
                idx += 2;
            }
        };

        // Fill in the fifo before we assert ROT_IRQ
        //
        // This provides a buffer while we wait for the interrupt
        // to go into our tight loop without the SP clocking out bytes
        // that are not yet present.
        write_fifo();

        self.assert_rot_irq();
        self.wait_for_csn_asserted();

        // This is a realtime loop for clocking out a full reply to the SP
        while !self.spi.ssd() {
            write_fifo();
        }

        //
        // We are done with our tight loop. Let's clean up and prepare for the
        // next request.
        //

        let result = self.wait_for_csn_deasserted();
        if result.is_err() {
            ringbuf_entry!(Trace::Desynchronized);
        }

        // If we weren't desynchronized, check for other errors
        if result.is_ok() {
            // Were any bytes clocked out?
            // We check to see if any bytes in the fifo have been sent or any have
            // been pushed into the fifo beyond the initial fill.
            if !self.spi.can_tx() && idx == ROT_FIFO_SIZE {
                // This was a CSn pulse
                // There's no need to flush here, since we de-assert ROT_IRQ at the
                // bottom of this function, which is the purpose of a flush.
                self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            } else {
                self.check_for_tx_error();
            }
        }

        self.cleanup();

        // Prime our write fifo, so we clock out zero bytes on the next receive
        // We also empty our read fifo, since we don't bother reading bytes while writing.
        self.prime_write_fifo_with_zeros();

        // Now that we are ready to handle the next request, let the SP know we
        // are ready.
        self.deassert_rot_irq();
    }

    pub fn prime_write_fifo_with_zeros(&mut self) {
        while self.spi.can_tx() {
            self.spi.send_u16(0);
        }
    }

    fn check_for_rx_error(&mut self) -> Result<(), IoError> {
        if self.spi.fifostat().rxerr().bit() {
            self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
            Err(IoError::Flow)
        } else {
            Ok(())
        }
    }

    // We don't actually want to return an error here.
    //
    // The SP will detect an underrun via a CRC error if the underrun occurred
    // during delivery of the reply message, or it will just be missed junk
    // after the message and won't matter.
    fn check_for_tx_error(&mut self) {
        if self.spi.fifostat().txerr().bit() {
            self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
            ringbuf_entry!(Trace::Underrun);
        }
    }

    fn assert_rot_irq(&self) {
        self.gpio.set_val(ROT_IRQ, Value::Zero);
    }

    fn deassert_rot_irq(&mut self) {
        self.gpio.set_val(ROT_IRQ, Value::One);
    }
}

// Return 2 bytes starting at `idx` combined into a u16 for putting on a fifo
// If `idx` >= `tx_buf.len()` use 0 for the byte.
fn get_u16(idx: usize, tx_buf: &[u8]) -> u16 {
    let upper = tx_buf.get(idx).copied().unwrap_or(0) as u16;
    let lower = tx_buf.get(idx + 1).copied().unwrap_or(0) as u16;
    upper << 8 | lower
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi);
    syscon.leave_reset(Peripheral::HsLspi);
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
