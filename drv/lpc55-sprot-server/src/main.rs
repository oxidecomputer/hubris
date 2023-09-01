// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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
//! before CSn is de-asserted, the message is malformed. The RoT will not
//! send an error response to the SP in this case, as it is not clear that the
//! channel is well-synchronized. Instead, the RoT will prepare for the next
//! request by calling `io.cleanup()`. The SP will timeout after not receiving a
//! ROT_IRQ to indicate a reply and retry.
//!
//! If for some reason there was a desynchronization, and the initial request
//! was not complete when the RoT saw an SSD bit set and went back to waiting
//! for the next request, then the SSD bit will have been from the prior SPI
//! transaction when we go to handle the next request on an SSA interrupt.
//! We can detect this scenario by waiting for an interrupt with only the SSD
//! bit set in the `intstat` register. In this case, we call `io.cleanup()`
//! again and go back to waiting for an SSA interrupt. It is ok if *both* SSA
//! and SSD bits are set as this just implies a short request that fits in a
//! FIFO. Note that without looking for a standalone SSD interrupt to indicate
//! desynchronization, and only reading the status register, we would not be
//! able to differentiate whether the SSD bit was already set before the SSA
//! interrupt or with it/after it for the new request. There is an inherent race
//! between clearing the status bit and getting an SSA interrupt where the SSD
//! bit could be set inbetween the clear and the SSA interrupt. Hence we use
//! a long SP retry and SSD interrupt for detection. See
//! https://github.com/oxidecomputer/hubris/issues/1507 for an example of why
//! this is necessary.
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
    sys_irq_control, sys_recv_closed, task_slot, TaskId, UnwrapLite,
};

mod handler;

use handler::Handler;

#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Trace {
    None,
    Dump(u32),
    ReceivedBytes(usize),
    RxHeader([u8; 6]),
    IoError(IoError),
    ReplyLen(usize),
    Underrun,
    Err(SprotProtocolError),
    Stats(RotIoStats),
    WaitForRequest,
    WaitForCsnAsserted,
    RotIrqAssert,
    RotIrqDeassert,
}
ringbuf!(Trace, 64, Trace::None);

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

    // We only want interrupts on CSn assert and CSn deassert
    //
    // Once we see an interrupt that contains the SSA bit we enter polling mode
    // and check the registers manually.
    //
    // If the interrupt only contains the SSD bit, then it means that we were
    // desynchronized. In this case we clear our fifos and go back to waiting
    // for the next interrupt.
    //
    // For us to be able to detect a standalone SSD from a prior request that we
    // started processing in the middle of, most likely due to an RoT reset (see
    // https://github.com/oxidecomputer/hubris/issues/1507), we must make sure
    // that the retry timeout at the SP is long enough so that the interrupt
    // fires before CSn is asserted on the next SP request.
    spi.ssa_enable();
    spi.ssd_enable();

    // Probably not necessary, drain Rx and Tx after config.
    spi.drain();

    // Disable the interrupts triggered by the `self.spi.drain_tx()`, which
    // unneccessarily causes spurious interrupts. We really only need to to
    // respond to CSn related interrupts, because after that we always enter a
    // tight loop or we skip over a stale CSn deasserted interrupt.
    spi.disable_tx();
    spi.disable_rx();

    Io {
        spi,
        gpio,
        stats: RotIoStats::default(),
        rot_irq_asserted: false,
    }
}

// Container for spi and gpio
struct Io {
    spi: crate::spi_core::Spi,
    gpio: drv_lpc55_gpio_api::Pins,
    stats: RotIoStats,

    /// This is an optimization to avoid talking to the GPIO task when we don't
    /// have to.
    /// ROT_IRQ is deasserted on startup in main.
    rot_irq_asserted: bool,
}

#[derive(Copy, Clone, PartialEq)]
enum IoError {
    Flush,
    Flow,
    // A special case of a flow control error, where the RoT started in the
    // middle of a request being clocked out and saw an SSD bit from a prior
    // request set. See https://github.com/oxidecomputer/hubris/issues/1507 for
    // more details.
    StaleSsd,
}

#[export_name = "main"]
fn main() -> ! {
    let mut io = configure_spi();

    let (rx_buf, tx_buf) = mutable_statics::mutable_statics! {
        static mut RX_BUF: [u8; REQUEST_BUF_SIZE] = [|| 0; _];
        static mut TX_BUF: [u8; RESPONSE_BUF_SIZE] = [|| 0; _];
    };

    let mut handler = Handler::new();

    loop {
        ringbuf_entry!(Trace::Stats(io.stats));
        match io.wait_for_request(rx_buf) {
            Ok(rx_len) => {
                if let Ok(rsp_len) =
                    handler.handle(&rx_buf[..rx_len], tx_buf, &mut io.stats)
                {
                    if let Err(err) = io.reply(&tx_buf[..rsp_len]) {
                        ringbuf_entry!(Trace::IoError(err));
                    }
                }
            }
            Err(IoError::Flush) => {
                // A flush indicates that the server should de-assert ROT_IRQ
                // as instructed by the SP. We do that and then proceed to wait
                // for the next request.
                ringbuf_entry!(Trace::IoError(IoError::Flush));

                // This should not actually be necessary, as it is done in
                // `io.cleanup()` above if the IRQ is actually asserted.
                // However, if for some reason  the GPIO driver crashed
                // and the de-assert failed, then this  gives us an extra
                // opportunity to clean things up. We *could* unconditionally
                // call `io.deassert_rot_irq` in `io.cleanup()`, but that adds
                // an extra call to the GPIO driver that we should be able to
                // avoid.
                io.deassert_rot_irq();
            }
            Err(err) => {
                ringbuf_entry!(Trace::IoError(err));
            }
        }
    }
}

impl Io {
    // Wait for chip select to be asserted
    fn wait_for_csn_asserted(&mut self) -> Result<(), IoError> {
        ringbuf_entry!(Trace::WaitForCsnAsserted);
        loop {
            sys_irq_control(notifications::SPI_IRQ_MASK, true);

            sys_recv_closed(
                &mut [],
                notifications::SPI_IRQ_MASK,
                TaskId::KERNEL,
            )
            .unwrap_lite();

            let intstat = self.spi.intstat();
            if intstat.ssd().bit() && !intstat.ssa().bit() {
                // This is a stale SSD bit, most likely from poor startup timing
                // of the RoT.
                //
                // See https://github.com/oxidecomputer/hubris/issues/1507
                self.stats.stale_ssd = self.stats.stale_ssd.wrapping_add(1);
                return Err(IoError::StaleSsd);
            }

            // Is CSn asserted by the SP?
            //
            // We specifically do *NOT* clear the ssd bit, as it is polled by
            // the calling code after this returns.
            if intstat.ssa().bit() {
                self.spi.ssa_clear();
                return Ok(());
            }
        }
    }

    pub fn wait_for_request(
        &mut self,
        rx_buf: &mut [u8],
    ) -> Result<usize, IoError> {
        ringbuf_entry!(Trace::WaitForRequest);
        self.prepare_for_request();
        self.wait_for_csn_asserted()?;

        // Go into a tight loop receiving as many bytes as we can until we see
        // CSn de-asserted.
        let mut bytes_received = 0;
        let mut rx = rx_buf.iter_mut();
        while !self.spi.ssd() {
            while self.spi.has_entry() {
                bytes_received += 2;
                let read = self.spi.read_u16();
                let upper = (read >> 8) as u8;
                let lower = read as u8;
                rx.next().map(|b| *b = upper);
                rx.next().map(|b| *b = lower);
            }
        }

        // There may be bytes left in the rx fifo after CSn is de-asserted
        while self.spi.has_entry() {
            bytes_received += 2;
            let read = self.spi.read_u16();
            let upper = (read >> 8) as u8;
            let lower = read as u8;
            rx.next().map(|b| *b = upper);
            rx.next().map(|b| *b = lower);
        }

        self.check_for_rx_error()?;

        if bytes_received == 0 {
            // This was a CSn pulse
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            return Err(IoError::Flush);
        }

        ringbuf_entry!(Trace::ReceivedBytes(bytes_received));
        ringbuf_entry!(Trace::RxHeader(rx_buf[0..6].try_into().unwrap()));

        Ok(bytes_received)
    }

    fn reply(&mut self, tx_buf: &[u8]) -> Result<(), IoError> {
        ringbuf_entry!(Trace::ReplyLen(tx_buf.len()));

        self.prepare_for_reply(tx_buf);
        self.assert_rot_irq();
        self.wait_for_csn_asserted()?;

        let mut idx = ROT_FIFO_SIZE;

        // If there is room in the fifo, then we must have transmitted some data.
        while !self.spi.ssd() {
            while self.spi.can_tx() {
                let entry = get_u16(idx, tx_buf);
                self.spi.send_u16(entry);
                idx += 2;
            }
        }

        // Detect a CSn pulse by seeing if any bytes were clocked out
        //
        // We check to see if any bytes in the fifo have been sent or any have
        // been pushed into the fifo beyond the initial fill.
        if !self.spi.can_tx() && idx == ROT_FIFO_SIZE {
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
        } else {
            self.check_for_tx_error();
        }

        Ok(())
    }

    // A function to cleanup internal state before we wait for the next request
    // from the SP.
    fn prepare_for_request(&mut self) {
        // Drain our TX and RX fifos
        self.spi.drain();

        // Ensure our write fifo is primed with zeroes, as we let the SPI block
        // clock these out for us while clocking in a request from the SP.
        self.prime_write_fifo_with_zeros();

        // Don't call out to the GPIO task unless necessary
        if self.rot_irq_asserted {
            self.deassert_rot_irq();
        }

        // Clear any errors
        self.spi.rxerr_clear();
        self.spi.txerr_clear();

        // Ensure our SSA/SSD bits are not set
        self.spi.ssa_clear();
        self.spi.ssd_clear();
    }

    // A function to cleanup internal state before we reply to the SP
    fn prepare_for_reply(&mut self, tx_buf: &[u8]) {
        // Drain our TX and RX fifos
        self.spi.drain();

        // Fill in the TX fifo before we assert ROT_IRQ
        let mut idx = 0;
        while self.spi.can_tx() {
            let entry = get_u16(idx, tx_buf);
            self.spi.send_u16(entry);
            idx += 2;
        }

        // Clear any errors
        self.spi.rxerr_clear();
        self.spi.txerr_clear();

        // Ensure our SSA/SSD bits are not set
        self.spi.ssa_clear();
        self.spi.ssd_clear();
    }

    fn prime_write_fifo_with_zeros(&mut self) {
        while self.spi.can_tx() {
            self.spi.send_u16(0);
        }
    }

    fn check_for_rx_error(&mut self) -> Result<(), IoError> {
        let fifostat = self.spi.fifostat();
        if fifostat.rxerr().bit() {
            self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
            Err(IoError::Flow)
        } else {
            Ok(())
        }
    }

    // We don't actually want to return an error here.
    // The SP will detect an underrun via a CRC error
    fn check_for_tx_error(&mut self) {
        let fifostat = self.spi.fifostat();

        if fifostat.txerr().bit() {
            // We don't do anything with tx errors other than record them.
            // The SP will see a checksum error if this is a reply, or the
            // underrun happened after the number of reply bytes and it
            // doesn't matter.
            self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
            ringbuf_entry!(Trace::Underrun);
        }
    }

    fn assert_rot_irq(&mut self) {
        self.gpio.set_val(ROT_IRQ, Value::Zero);
        self.rot_irq_asserted = true;
        ringbuf_entry!(Trace::RotIrqAssert);
    }

    fn deassert_rot_irq(&mut self) {
        self.gpio.set_val(ROT_IRQ, Value::One);
        self.rot_irq_asserted = false;
        ringbuf_entry!(Trace::RotIrqDeassert);
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
