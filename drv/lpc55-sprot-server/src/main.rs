// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!

#![no_std]
#![no_main]

mod handler;

use drv_lpc55_gpio_api::{Direction, Pin, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_sprot_api::*;
use lpc55_pac as device;

use crc::{Crc, CRC_32_CKSUM};
use lpc55_romapi::bootrom;
use userlib::*;

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

// See drv/sprot-api/README.md
// Messages are received from the Service Processor (SP).
//
// Messages are framed at the physical layer by the SP asserting the
// `Chip Select` (negated) (`CSn`) signal.
//
// Responses to messages from the SP are not available until ROT_IRQ is
// asserted by the RoT.
//
// The RoT has control of the ROT_IRQ line which is used to indicate that the
// RoT has a message ready for the SP to read.
//
// The RoT will always process incoming bytes even if it is transmitting a
// previously queued message.
//
// Messages for protocol 1 are composed of a four-byte header:
//   - a protocol version (0x01)
//   - the lease significant byte of the payload length
//   - the most significant byte of the payload length
//   - a message type
//   - the payload, not to exceed a maximum length
//
// If the payload length exceeds the maximum size or not all bytes are received
// before CSn is de-asserted, the message is malformed and an error message
// will be queued if Tx is idle.
//
// RoT Tx messages will not be written to the Tx FIFO until CSn is de-asserted.
// ROT_IRQ will not be asserted until data has been written to the Tx FIFO.
// This allows the RoT to take an arbitrary amount of time to prepare a
// response message and allows the RoT to make a best effort to meet
// the real time requirements of feeding the Tx FIFO when the SP clocks out
// the response message.
//
// ROT_IRQ is expected to be an edge triggered interrupt on the SP.
// ROT_IRQ is de-asserted only after all Tx bytes have drained.
// That gives a bit more information when looking at logic analyzer traces.
//
// Always setup to transfer the full Tx buffer contents to SP
// even if it is longer than the valid message or if there is no valid message.
// This keeps the inner IO loop simple.
// Ensure that there are no bytes from any previous Tx message still in
// the Tx buffer.
//

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum IoStatus {
    Flush,
    IOResult { overrun: bool, underrun: bool },
}

#[repr(C, align(8))]
struct IO {
    spi: crate::spi_core::Spi,
    gpio: drv_lpc55_gpio_api::Pins,
    // Transmit Buffer
    tx: [u8; RSP_BUF_SIZE],
    // Receive Buffer
    rx: [u8; REQ_BUF_SIZE],
    // Number of bytes copied into the receive buffer
    rxcount: usize,
    // Number of bytes copied from the transmit buffer into the FIFO
    txcount: usize,
    // Number of bytes left in FIFOWR at the end of the frame.
    txremainder: usize,
    // Failed to keep up with FIFORD
    overrun: bool,
    // Failed to keep up with FIFOWR
    underrun: bool,
}

struct Server<'a> {
    io: &'a mut IO,
    status: &'a mut Status,
    handler: &'a mut handler::Handler,
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
                  // If there were any HW interrupts pending, clear those conditions.
                  // The kernel may still deliver a "spurious" interrupt.
                  // spi.rxerr_clear();
                  // spi.txerr_clear();

    // Take interrupts on CSn asserted and de-asserted.
    // spi.ssa_clear();
    // spi.ssd_clear();

    // spi.enable_rx(); // Enable FIFO Rx interrupts
    // spi.enable_tx(); // Enable FIFO Tx interrupts
    spi.ssa_enable(); // Interrupt on CSn changing to asserted.
    spi.ssd_enable(); // Interrupt on CSn changing to deasserted.
    spi.drain(); // Probably not necessary, drain Rx and Tx after config.

    pub const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_CKSUM);
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

    let mut io = IO {
        tx: [0u8; RSP_BUF_SIZE],
        rx: [0u8; REQ_BUF_SIZE],
        gpio,
        spi,
        rxcount: 0,
        txcount: 0,
        txremainder: 0,
        overrun: false,
        underrun: false,
    };
    let mut status = Status {
        supported: 1_u32 << VERSION_1,
        bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
        fwversion: 0, // TODO: Put something useful in here.
        buffer_size: REQ_BUF_SIZE as u32,
        rx_received: 0,
        rx_overrun: 0,
        tx_underrun: 0,
        rx_invalid: 0,
        tx_incomplete: 0,
        handler_error: 0,
    };

    let server = &mut Server {
        io: &mut io,
        status: &mut status,
        handler: &mut handler::new(),
    };

    // Process a null message as if it had been just received.
    // Expect that the Tx buffer is cleared.
    server.handler.handle(
        false,
        crate::IoStatus::Flush,
        &server.io.rx,
        &mut server.io.tx,
        0,
        server.status,
    );
    let mut transmit = false;

    loop {
        server.io.pio(transmit);
        transmit = match server.handler.handle(
            transmit, // true if previous loop transmitted.
            IoStatus::IOResult {
                underrun: server.io.underrun,
                overrun: server.io.overrun,
            },
            &server.io.rx,
            &mut server.io.tx,
            server.io.rxcount,
            server.status,
        ) {
            Some(_txlen) => true,
            None => false,
        }
    }
}

impl IO {
    /// Wait for chip select to be asserted then service the FIFOs until end of frame.
    /// Returns false on spurious interrupt.
    fn pio(&mut self, transmit: bool) {
        let tx_end = self.tx.len(); // Available bytes and trailing zeros
        let rx_end = self.rx.len(); // All of the available bytes
        self.txcount = 0;
        self.rxcount = 0;
        self.txremainder = 0;
        self.overrun = false;
        self.underrun = false;

        if !transmit {
            // Ensure that unused Tx buffer is zero-filled.
            // TODO: Is there a way to do this that is more efficient while in
            // critical real-time sections? DMA may require this too.
            self.tx.fill(0);
        }

        // Prime FIFOWR in order to be ready for start of frame.
        //
        // All our interrupts are left enabled for the sake of simplicity.
        // The downside is that the following drain will elicit a spurious
        // interrupt. But, that interrupt will occur before the start of any
        // frame when we are not in a realtime situation.
        self.spi.drain_tx(); // FIFOWR is now empty; we'll get an interrupt.
        loop {
            if self.txcount >= tx_end || !self.spi.can_tx() {
                break;
            }
            // TODO don't generate panic code below.
            let b = self.tx[self.txcount];
            self.spi.send_u8(b);
            self.txcount += 1;
        }

        if transmit {
            self.gpio.set_val(ROT_IRQ, Value::Zero).unwrap_lite();
        } else {
            self.gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
        }
        sys_irq_control(1, true);
        sys_recv_closed(&mut [], 1, TaskId::KERNEL).unwrap_lite();

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
                    // TODO: It would be nice to eliminate the spurious interrupt
                    // through careful disablement/enablement of certain interrupts.
                    // If we could "peek" for posted interrupt in the kernel and clear it
                    // Would that open us up to missing start of frame in some corner case?
                    continue 'outer;
                }
                // There is work to do.

                // Track Spi Select Asserted and Deasserted to determine if
                // we are in freame and update Rx/Tx state as needed.
                if intstat.ssa().bit() {
                    self.spi.ssa_clear();
                    inframe = true;
                }

                // Note that while Tx is done at end of frame,
                // FIFORD may still have bytes to read.
                if intstat.ssd().bit() {
                    self.spi.ssd_clear();
                    // ROT_IRQ will only be asserted if there was a Tx
                    // message from RoT to SP.
                    // For simplicity, deassert unconditionally.
                    if self.gpio.set_val(ROT_IRQ, Value::One).is_err() {
                        // XXX this can never happen
                        return;
                    }
                    inframe = false;
                    // TODO: Is it interesting to keep track of the exact number of bytes
                    // sent and received? Tx bytes left in FIFOWR would be part of that
                    // calculation.
                }

                // Note that `fifostat` is fresh from waking from interrupt or
                // the re-read at the end of this loop.
                // Check for Rx overrun.
                if fifostat.rxerr().bit() {
                    self.spi.rxerr_clear();
                    self.overrun = true;
                    // Overrun accounting is done in handler.
                }
                if fifostat.txerr().bit() {
                    self.spi.txerr_clear();
                    // Underrun accounting is done in handler.
                    self.underrun = true;
                }
                // Service the FIFOs
                //   - inframe: normal service
                //   - !inframe: this is the last service needed for FIFORD
                loop {
                    let mut io = false;
                    if self.spi.can_tx() {
                        let (b, incr) = if self.txcount < tx_end {
                            io = true;
                            (self.tx[self.txcount], 1)
                        } else {
                            (0, 0)
                        };
                        self.spi.send_u8(b);
                        self.txcount += incr;
                    }
                    if self.spi.has_byte() {
                        let b = self.spi.read_u8();
                        let incr = if self.rxcount < rx_end {
                            io = true;
                            self.rx[self.rxcount] = b;
                            1
                        } else {
                            0
                        };
                        self.rxcount += incr;
                    }
                    if !io {
                        break;
                    }
                }
                if !inframe {
                    // If CSn was deasserted, then the IO loop just completed
                    // would have fetched the remaining bytes out of FIFORD
                    // and our work is done.
                    // We're sending out trailing bytes in FIFOWR to avoid
                    // Tx underrun errors when we get more Rx bytes than we
                    // are sending. So, expect that there are always some bytes
                    // remaining in FIFOWR.
                    // Actual transmitted bytes are always going to equal the
                    // number of received bytes. Since the rx_count is not
                    // scewed by the Tx trailing bytes still in FIFOWR, just
                    // use rx_count for both.

                    // let fifostat = self.spi.fifostat();

                    // Update Tx state and account for trailing bytes
                    // left in FIFOWR if present.
                    self.txremainder = fifostat.txlvl().bits() as usize;
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
        self.spi.send_u8(VERSION_BUSY);
        self.gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi).unwrap_lite();
    syscon.leave_reset(Peripheral::HsLspi).unwrap_lite();

    syscon.enable_clock(Peripheral::Fc3).unwrap_lite();
    syscon.leave_reset(Peripheral::Fc3).unwrap_lite();
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
