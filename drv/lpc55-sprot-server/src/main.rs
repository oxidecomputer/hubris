// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!

#![no_std]
#![no_main]

mod handler;

use drv_lpc55_gpio_api::{Direction, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_sprot_api::{Protocol, RxMsg2, TxMsg2, VerifiedTxMsg2, BUF_SIZE};
use lpc55_pac as device;

use crc::{Crc, CRC_32_CKSUM};
use lpc55_romapi::bootrom;
use ringbuf::*;
use userlib::*;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Overrun(usize),
    Pio(bool),
    RotIrqAssert,
    RotIrqDeassert,
    Underrun(usize),
    SpiSsd,
    SpiSsa,
    Busy,
}
ringbuf!(Trace, 32, Trace::None);

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

// Notification mask for Flexcomm8 hs_spi IRQ; must match config in app.toml
const SPI_IRQ: u32 = 1;

// See drv/sprot-api/README.md
// Messages are received from the Service Processor (SP) over a SPI interface.
//
// The RoT indicates that a response is ready by asserting ROT_IRQ to the SP.
//
// It is possible for the SP to send a new message to the SP while receiving
// the RoT's reponse to the SP's previous message.
//
// See drv/sprot-api for message layout.
//
// If the payload length exceeds the maximum size or not all bytes are received
// before CSn is de-asserted, the message is malformed and an ErrorRsp message
// will be sent to the SP.
//
// Messages from the SP are not processed until the SPI chip-select signal
// is deasserted.
//
// ROT_IRQ is intended to be an edge triggered interrupt on the SP.
// ROT_IRQ is de-asserted only after CSn is deasserted.
//
// The RoT sets up to transfer the full Tx buffer contents to SP
// even if it is longer than the valid message or if there is no valid message.
// Extra bytes are set to zero.
// This keeps the inner IO loop simple and ensures that there are no bytes from
// any previous Tx message still in the Tx buffer.
//

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum IoStatus {
    Flush,
    IOResult { overrun: bool, underrun: bool },
}

#[repr(C)]
struct IO {
    spi: crate::spi_core::Spi,
    gpio: drv_lpc55_gpio_api::Pins,
    // Failed to keep up with FIFORD
    overrun: bool,
    // Failed to keep up with FIFOWR
    underrun: bool,
}

struct Server<'a> {
    io: &'a mut IO,
    state: &'a mut LocalState,
    handler: &'a mut handler::Handler,
}

// A combination of things generated and/or mutated by the the lpc55 side of sprot
// and returned from the `drv_sprot_api::Status` and `drv_sprot_api::IoStats` types.
//
// Comments copied directly from the `drv_sprot_api` types.
//
// This type exists as a mechanism to split the functionality provided by the
// previous `Status` message into two messages returned by two different API
// calls, without having to change a lot of other things right now.
pub(crate) struct LocalState {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    supported: u32,
    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    /// TODO: This should live in the stage0 handoff info
    bootrom_crc32: u32,

    /// Maxiumum message size that the RoT can handle.
    buffer_size: u32,
    /// Number of messages received
    pub rx_received: u32,

    /// Number of messages where the RoT failed to service the Rx FIFO in time.
    pub rx_overrun: u32,

    /// Number of messages where the RoT failed to service the Tx FIFO in time.
    pub tx_underrun: u32,

    /// Number of invalid messages received
    pub rx_invalid: u32,

    /// Number of incomplete transmissions (valid data not fetched by SP).
    pub tx_incomplete: u32,
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    let gpio_driver = GPIO.get_task_id();
    setup_pins(gpio_driver).unwrap_lite();
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);
    // TODO: It should never happen but, initialization should be able to deal
    // with CSn being asserted at init time.

    // Configure ROT_IRQ
    // Ensure that ROT_IRQ is not asserted
    gpio.set_dir(ROT_IRQ, Direction::Output).unwrap_lite();
    gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
    ringbuf_entry!(Trace::RotIrqDeassert);

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
    spi.enable(); // Drain and configure FIFOs
    spi.ssa_enable(); // Interrupt on CSn changing to asserted.
    spi.ssd_enable(); // Interrupt on CSn changing to deasserted.
    spi.drain(); // Probably not necessary, drain Rx and Tx after config.

    pub const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_CKSUM);
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

    // Receive buffer
    let mut rx_buf = [0u8; BUF_SIZE];
    let mut tx_buf = [0u8; BUF_SIZE];

    let mut io = IO {
        gpio,
        spi,
        overrun: false,
        underrun: false,
    };

    let mut state = LocalState {
        supported: 1_u32 << (Protocol::V1 as u8),
        bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
        buffer_size: BUF_SIZE as u32,
        rx_received: 0,
        rx_overrun: 0,
        tx_underrun: 0,
        rx_invalid: 0,
        tx_incomplete: 0,
    };

    let server = &mut Server {
        io: &mut io,
        state: &mut state,
        handler: &mut handler::new(),
    };

    // Create an VerifiedTxMsg2 full of 0s to start the loop with
    let mut verified_tx_msg: Option<VerifiedTxMsg2> = None;

    loop {
        let mut rx_msg = RxMsg2::new(&mut rx_buf[..]);
        if verified_tx_msg.is_none() {
            verified_tx_msg = Some(TxMsg2::new(&mut tx_buf[..]).zeroes());
        }
        server
            .io
            .pio(verified_tx_msg.as_mut().unwrap_lite(), &mut rx_msg);
        let iostat =
            if rx_msg.len() == 0 && !server.io.underrun && !server.io.overrun {
                IoStatus::Flush
            } else {
                IoStatus::IOResult {
                    underrun: server.io.underrun,
                    overrun: server.io.overrun,
                }
            };

        verified_tx_msg = server.handler.handle(
            iostat,
            rx_msg,
            verified_tx_msg.unwrap_lite(),
            server.state,
        );
    }
}

impl IO {
    /// Wait for chip select to be asserted then service the FIFOs until end of frame.
    /// Returns false on spurious interrupt.
    fn pio(&mut self, tx_msg: &mut VerifiedTxMsg2, rx_msg: &mut RxMsg2) {
        self.overrun = false;
        self.underrun = false;
        let transmit = tx_msg.contains_data();
        let mut tx_iter = tx_msg.iter();

        ringbuf_entry!(Trace::Pio(transmit));

        // Prime FIFOWR in order to be ready for start of frame.
        //
        // All our interrupts are left enabled for the sake of simplicity.
        // The downside is that the following drain will elicit a spurious
        // interrupt. But, that interrupt will occur before the start of any
        // frame when we are not in a realtime situation.
        self.spi.drain_tx(); // FIFOWR is now empty; we'll get an interrupt.
        loop {
            if !self.spi.can_tx() {
                break;
            }
            match tx_iter.next() {
                Some(b) => self.spi.send_u8(b),
                None => break, // FIFOWR is full
            }
        }

        if transmit {
            ringbuf_entry!(Trace::RotIrqAssert);
            self.gpio.set_val(ROT_IRQ, Value::Zero).unwrap_lite();
        }

        // TODO: SP RESET needs to be monitored, otherwise, any
        // forced looping here could be a denial of service attack against
        // observation of SP resetting. SP resetting without invalidating
        // security related state means a compromised SP could operate using
        // the trust gained in the previous session.
        // Upper layers may mitigate that, but check on it.

        // Wait for chip select to be asserted and perform all subsequent I/O
        // for that frame.
        // Track the state of chip select (CSn)
        let mut inframe = false;
        'outer: loop {
            // restart here on the one expected spurious interrupt.
            sys_irq_control(SPI_IRQ, true);
            sys_recv_closed(&mut [], SPI_IRQ, TaskId::KERNEL).unwrap_lite();
            loop {
                // Get frame start/end interrupt from intstat (SSA/SSD).
                let intstat = self.spi.intstat();
                let fifostat = self.spi.fifostat();

                // During bulk data transfer, we'll be polling and not servicing
                // the interrupts that the kernel is collecting.
                // As a result, there can be a left-over kernel interrupt to handle
                // even after the HW interrupts are retired.
                if !intstat.ssa().bit()
                    && !intstat.ssd().bit()
                    && !fifostat.txnotfull().bit()
                    && !fifostat.rxnotempty().bit()
                    && !inframe
                {
                    // This is a spurious interrupt.
                    // These are only happening just after queuing a
                    // response.
                    // TODO: It would be nice to eliminate the spurious interrupt.
                    continue 'outer;
                }

                // Track Spi Select Asserted and Deasserted to determine if
                // we are in frame and update Rx/Tx state as needed.
                if intstat.ssa().bit() {
                    ringbuf_entry!(Trace::SpiSsa);
                    self.spi.ssa_clear();
                    inframe = true;
                }

                // Note that while Tx is done at end of frame,
                // FIFORD may still have bytes to read.
                if intstat.ssd().bit() {
                    ringbuf_entry!(Trace::SpiSsd);
                    self.spi.ssd_clear();
                    if transmit {
                        self.gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
                    }
                    inframe = false;
                }

                // Note that `fifostat` is fresh from waking from interrupt or
                // the re-read at the end of this loop.
                // Check for Rx overrun.
                if fifostat.rxerr().bit() {
                    self.spi.rxerr_clear();
                    self.overrun = true;
                    ringbuf_entry!(Trace::Overrun(rx_msg.len()));
                    // Overrun accounting is done in handler.
                }
                if fifostat.txerr().bit() {
                    self.spi.txerr_clear();
                    // Underrun accounting is done in handler.
                    ringbuf_entry!(Trace::Underrun(rx_msg.len()));
                    self.underrun = true;
                }
                // Service the FIFOs
                //   - inframe: normal service
                //   - !inframe: this is the last service needed for FIFORD
                let mut io = true;
                while io == true {
                    io = false;
                    if self.spi.can_tx() {
                        if let Some(b) = tx_iter.next() {
                            io = true;
                            self.spi.send_u8(b);
                        }
                    }
                    if self.spi.has_byte() {
                        let b = self.spi.read_u8();
                        if rx_msg.push(b).is_ok() {
                            io = true;
                        }
                    }
                }
                if !inframe {
                    // If CSn was deasserted, then the IO loop that was just
                    // completed would have fetched the remaining bytes out of
                    // FIFORD and our work is done.
                    //
                    // The SP is allowed to send a long message to us at the
                    // same time it is retrieving a short response from us.
                    //
                    // We keep FIFOWR full with zeros when we run out of Tx data
                    // in order to avoid Tx underrun errors.
                    //
                    // So, there will always be "unsent" bytes in FIFOWR.
                    //
                    // Actual transmitted bytes are always going to equal the
                    // number of received bytes. Since the rx_count is not
                    // skewed by the Tx trailing bytes still in FIFOWR, just
                    // use rx_count for both.

                    // Update Tx state and account for trailing bytes
                    // left in FIFOWR if present.
                    //
                    // If we cared about those remaining FIFOWR bytes:
                    // let txremainder = fifostat.txlvl().bits() as usize;
                    break 'outer;
                }
            } // FIFO polling loop
        }
        // Done, deassert ROT_IRQ

        // XXX Denial of service by forever asserting CSn?
        // We could mitigate by imposing a time limit
        // and resetting the SP if it is exceeded.
        // But, the management plane is going to notice that
        // the RoT is not available. So, does it matter?

        // Any data remaining in FIFORD that could be comsumed,
        // has been consumed. So, close out the received message.

        // The following drain should be redundant with the one at the
        // beginning of this function.
        // However, if SP sends/receives while we're away processing
        // a message, then we will see an underrun, and possibly an
        // overrun, when we come back instead of just a Tx not full interrupt.
        self.spi.drain_tx();
        // Put a Protocol::Busy value in FIFOWR so that SP/logic analyzer knows we're away.
        ringbuf_entry!(Trace::Busy);
        self.spi.send_u8(Protocol::Busy as u8);
        if transmit {
            self.gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
            ringbuf_entry!(Trace::RotIrqDeassert);
        }
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi).unwrap_lite();
    syscon.leave_reset(Peripheral::HsLspi).unwrap_lite();
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
