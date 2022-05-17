// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!

#![no_std]
#![no_main]

use drv_lpc55_gpio_api::{Direction, Pin, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_spi_msg::*;
use lpc55_pac as device;
use ringbuf::*;
use userlib::*;

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

// A SPI target device is implemented.
// Messages are received from the Service Processor (SP).
//
// Messages are framed at the physical layer by the SP asserting the
// `Chip Select` (negated) or `CSn` signal.
//
// Although SPI supports full duplex communications, the messages carried
// between the SP and the RoT may take significant time for the RoT to process.
// Therefore, some form of flow control is needed.
// The crudest form would be for the SP to sleep for a set time after
// transmitting a request. That would be inefficient and may not adapt well
// as use cases develop.
// The RoT has control of the ROT_IRQ line which is used to indicate that the
// RoT has a message ready for the SP to read.
//
// Messages have a protocol identifier as the first byte for forward
// compatibility. Protocol 0x01 is the first supported version.
// The RoT will ignore messages that begin with a 0x00 byte.
// Ill formed messages will elicit an error response message on the next read
// unless a pending message is already queued.
//
// The SP may loose synchronization or choose to ignore any previous
// session state that may exist. To support that semantic, the RoT will always
// process incoming bytes even if it is transmitting a previously queued
// message. The transmit queue is only one-deep at the time of writing.
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
//   - TODO: should ROT_IRQ be de-asserted after some minimal hold time or
//   - TODO: should ROT_IRQ be de-asserted one byte has been transmitted to SP or
//   - TODO: should ROT_IRQ be de-asserted only after all Tx bytes have drained?
//
// Intiial supported messages include a simple Echo and EchoReturn message to
// test the interface and support for Sprockets.

#[derive(Copy, Clone, PartialEq)]
#[repr(u8)]
enum RxState {
    Idle = 0,       // Idle, waiting for message
    Header = 1,     // Expecting message length LSB, MSB, and MsgType
    Payload = 2,    // receiving message payload
    Dispatch = 3,   // Receive complete, message processing is pending.
    Responding = 4, // Tx frame is queued. Terminal state until next SOT.
    Invalid = 5,    // bad message or normally ignoring incoming data until SOT.
    Error = 0xff,   // Errors, Terminal state until next SOT.
}

impl From<u8> for RxState {
    fn from(rxstate: u8) -> Self {
        match rxstate {
            0 => RxState::Idle,
            1 => RxState::Header,
            2 => RxState::Payload,
            3 => RxState::Dispatch,
            4 => RxState::Responding,
            5 => RxState::Invalid,
            _ => RxState::Error,
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
#[repr(u8)]
enum TxState {
    Idle = 0,     // Idle, waiting for message to be queued.
    Queued = 1,   // Message queued, waiting for start of Tx frame.
    Writing = 2,  // Message is transferring from local buffer to FIFO.
    Finish = 3,   // Final bytes are being transmitted from FIFO to SPI bus.
                  // Last byte has been clocked out to SPI bus, return to Idle.
    Error = 0xff, // Error during transmit.
}

impl From<u8> for TxState {
    fn from(txstate: u8) -> Self {
        match txstate {
            0 => TxState::Idle,
            1 => TxState::Queued,
            2 => TxState::Writing,
            3 => TxState::Finish,
            _ => TxState::Error,
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    AlreadyEnqueuedMsg,
    BadMsgLength,
    CannotSendRxFragError,
    CollectPayload,
    DeassertRotIrq,
    Enqueued,
    EnqueuedMsg(usize),
    EnqueueFail,
    EnqueueTooBig,
    GetThePayload,
    HeaderCollect,
    HeaderComplete,
    HeaderOnly,
    Initialized,
    InvalidRxState(RxState),
    Irq(bool, bool, bool),
    Line,
    EndOfFrame,
    SsaClear,
    SsdClear,
    Loop(u16, u16, TxState, RxState),
    LStat(bool, bool, bool, bool, bool, bool, bool, u8, u8),
    PayloadComplete,
    ResetRxToIdle,
    Responding,
    RespondToEcho,
    RespondToSprockets,
    RespondToUnknown,
    RxFragment,
    // RxIntDisable,
    // RxIntEnable,
    Rx(RxState, u8, usize, bool, bool),
    RxState(RxState),
    // S1Deasserted,
    // Start,
    // Stat(u32),
    // TxIntDisable,
    // TxIntEnable,
    Tx(TxState, u8, usize),
    StartTx(bool, bool),
    None,
}

ringbuf!(Trace, 128, Trace::None);

struct TxContext<'a> {
    state: TxState,
    tx: &'a mut [u8],
    count: usize, // index for transmit
    end: usize, // end of packet index
    rot_irq: bool, // Track ROT_IRQ state
    inframe: bool,  // true when SP has CSn asserted.
}

struct RxContext<'a> {
    state: RxState,
    rx: &'a mut [u8],
    count: usize, // cursor for receive
    end: usize, // cursor for receive
}

impl<'a> TxContext<'a> {
    pub fn new(tx: &'a mut [u8]) -> Self {
        Self {
            state: TxState::Idle,
            tx,
            count: 0,
            end: 0,
            rot_irq: false,
            inframe: false,
        }
    }

    pub fn enqueue(&mut self, msgtype: MsgType, buf: Option<&[u8]>) -> bool {
        if self.state != TxState::Idle {
            ringbuf_entry!(Trace::AlreadyEnqueuedMsg);
            return false;
        }
        let len = match buf {
            Some(buf) => { buf.len() },
            None => { 0 },
        };
        if len > self.tx.len() {
            ringbuf_entry!(Trace::EnqueueTooBig);
            return false;
        }

        let mut msg = Msg::parse(&mut *self.tx).unwrap_lite();
        msg.set_version();
        msg.set_len(len);
        msg.set_msgtype(msgtype);
        if let Some(buf) = buf {
            let out = msg.payload_buf();
            out[..len].clone_from_slice(&buf[..len]);
        }
        self.state = TxState::Queued;
        self.count = 0;
        self.end = SPI_HEADER_SIZE + len;
        ringbuf_entry!(Trace::EnqueuedMsg(self.end));
        true
    }
}

impl<'a> RxContext<'a> {
    pub fn new(rx: &'a mut [u8]) -> Self {
        Self {
            state: RxState::Idle,
            rx,
            count: 0,
            end: 0, // Set to expected fixed length message or rx.len()
        }
    }

    // Set Rx idle and ready to receive.
    pub fn ready(&mut self) {
        let mut msg = Msg::parse(&mut *self.rx).unwrap_lite();
        msg.set_msgtype(drv_spi_msg::MsgType::Invalid);
        let len = msg.payload_buf().len();
        self.state = RxState::Idle;
        ringbuf_entry!(Trace::ResetRxToIdle);
        self.count = 0;
        self.end = len;
    }

    /// Process an input byte.
    /// A start of transmission condition resets the receive state machine.
    /// Returns an updated "again" as true if looping should continue.
    pub fn rx_byte(&mut self, b: u8, sot: bool, tx: &mut TxContext) {
        // SP can issue a command (or no-op) at any start of transmission event.
        if sot {
            ringbuf_entry!(Trace::RxState(self.state)); // log old state
            self.ready();
        }

        match self.state {
            RxState::Invalid => {
                // Ignoring bytes due to previous error or completion of Rx.
            }
            RxState::Idle => {
                // The first received byte must be a supported protocol number.
                self.rx[self.count] = b;
                self.count += 1;
                match b {
                    SPI_MSG_IGNORE => {
                        // A zero byte marks the message as one to ignore.
                        self.state = RxState::Invalid;
                    }
                    SPI_MSG_VERSION => {
                        // The only supported protocol at this time is 0x01.
                        // Collect a header.
                        self.state = RxState::Header;
                    }
                    _ => {
                        // Form an error response unless Tx is sending something
                        // valid.
                        if tx.enqueue(MsgType::Error, Some(&self.rx[0..1])) {
                            ringbuf_entry!(Trace::Enqueued);
                            self.state = RxState::Dispatch; // or RxState::Error?
                            // TODO: a documented error type should be the first byte.
                        } else {
                            // We received garbage at sot but Tx was not idle.
                            ringbuf_entry!(Trace::EnqueueFail);
                        }
                    }
                }
            }
            RxState::Header => {
                self.rx[self.count] = b;
                self.count += 1;
                ringbuf_entry!(Trace::HeaderCollect);
                if self.count == SPI_HEADER_SIZE {
                    ringbuf_entry!(Trace::HeaderComplete);
                    let max_len = self.rx.len();
                    let msg = Msg::parse(&mut *self.rx).unwrap_lite();
                    self.end = msg.payload_len() + SPI_HEADER_SIZE;
                    if self.end > max_len {
                        ringbuf_entry!(Trace::BadMsgLength);
                        self.state = RxState::Error;
                        //let mut buf = [0u8; SPI_HEADER_SIZE];
                        //buf[..SPI_HEADER_SIZE].clone_from_slice(&self.rx[..SPI_HEADER_SIZE]);

                        if tx.enqueue(MsgType::Error,
                            Some(&self.rx[0..SPI_HEADER_SIZE])) {
                            self.state = RxState::Dispatch;
                        }
                    } else if self.end == SPI_HEADER_SIZE {
                        self.state = RxState::Dispatch; // zero-length payload
                        ringbuf_entry!(Trace::HeaderOnly);
                    } else {
                        self.state = RxState::Payload;
                        ringbuf_entry!(Trace::GetThePayload);
                    }
                }
            }
            RxState::Payload => {
                self.rx[self.count] = b;
                self.count += 1;
                ringbuf_entry!(Trace::CollectPayload);
                if self.count == self.end {
                    ringbuf_entry!(Trace::PayloadComplete);
                    self.state = RxState::Dispatch;
                }
            }
            _ => {
                // The remaining states don't care about excess bytes.
                ringbuf_entry!(Trace::InvalidRxState(self.state));
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(SYSCON.get_task_id());

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_flexcomm(&syscon);

    let gpio_driver = GPIO.get_task_id();

    setup_pins(gpio_driver).unwrap_lite();
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

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
    sys_irq_control(1, true);

    // Configure SP_IRQ
    let sp_irq = Pin::PIO0_18; // XXX Should be in app.toml
    // Direction must be set after other pin configuration.
    if gpio.set_dir(sp_irq, Direction::Output).is_err() {
        panic!();
    }
    // At the begining, we are ready to receive and have nothing to transmit.
    // Ensure that ROT_IRQ is not asserted
    if gpio.set_val(sp_irq, Value::One).is_err() {
        panic!();
    }

    // Configure SP_RESET
    // XXX Reset is normally an input to detect SP internal reset
    // XXX but can be an driven as an output to reset the SP.
    // XXX SP reset and reset detection should be elsewhere.
    let sp_reset = Pin::PIO0_9; // XXX Should be in app.toml
    if gpio.set_dir(sp_reset, Direction::Input).is_err() {
        panic!();
    }
    // TODO: Detect SP reset events and invalidate any trust/session that may exist.
    // TODO: Drive SP reset when appropriate.
    // Because detect/invalidate function is associated with the SPDM/Sprockets state, the
    // implementation could be here.

    let mut tx = [0u8; SPI_BUFFER_SIZE];
    let mut tctx = &mut TxContext::new(&mut tx[..]);
    let mut rx = [0u8; SPI_BUFFER_SIZE];
    let mut rctx = &mut RxContext::new(&mut rx[..]);

    spi.enable_rx();
    spi.disable_tx();   // Ignore Tx interrupts for now.
    spi.send_u8(0x00);  // Throw away data, but this sets bit-level framing.
    spi.drain();        // Nothing in the FIFOs is interesting yet.
    spi.rxerr_clear();
    // spi.txerr_clear();

    // Take interrupts on CSn asserted and de-asserted.
    // The assert interrupt happens before the first byte of Rx data is
    // available. That could make us more reponsive if keeping up with realtime
    // requirements becomes an issue.
    // On de-assert, we'll process any received message and then queue up a
    // response for the next CSn assert.
    spi.ssa_enable();    // Interrupt on CSn changing to asserted.
    spi.ssd_enable();    // Interrupt on CSn changing to deasserted.
    ringbuf_entry!(Trace::Initialized); // XXX

    let mut olc = 0u16;
    loop {
        if sys_recv_closed(&mut [], 1, TaskId::KERNEL).is_err() {
            panic!()
        }

        // Note: SP RESET needs to be monitored, otherwise, any
        // forced looping here could be a denial of service attack against
        // observation of SP resetting. SP resetting without invalidating
        // security related state means a compromised SP could operate using
        // the trust gained in the previous session.
        // Upper layers may mitigate that, but check on it.
        olc = olc.wrapping_add(1u16);
        let mut ilc = 0u16;
        loop {
            ilc = ilc.wrapping_add(1u16);
            let mut again = false;

            let (ssa, ssd, mstidle) = spi.intstat();
            if ssa {
                ringbuf_entry!(Trace::SsaClear);
                spi.ssa_clear();  
                // We could call rctx.ready() here,
                // but the sot flag serves that purpose.
                // Fielding the SSA interrupt may get us ready sooner.
                // TODO: measure interrupt latency to process first Rx byte.
                // XXX YY again = true;
                tctx.inframe = true;
            }
            if ssd {
                ringbuf_entry!(Trace::SsdClear);
                spi.ssd_clear();
                tctx.inframe = false;
            }

            // ringbuf_entry!(Trace::Stat(spi.get_fifostat()));
            let (txerr, rxerr, perint, txempty, txnotfull, rxnotempty, rxfull,
                txlvl, rxlvl) = spi.stat();

            if ssa || ssd || txerr || rxerr || perint || txempty || txnotfull || rxnotempty || rxfull || txlvl != 8 || rxlvl != 0 {
                ringbuf_entry!(Trace::Loop(olc, ilc, tctx.state, rctx.state));
                ringbuf_entry!(Trace::Irq(ssa, ssd, mstidle));
                ringbuf_entry!(Trace::LStat(txerr, rxerr, perint, txempty,
                    txnotfull, rxnotempty, rxfull, txlvl, rxlvl));
            }

            // TODO: catch aborted transmissions.
            //       state == writing and ssd

            // Messages from the RoT to SP do not begin writing to FIFOWR
            // until the SPI bus is idle (CSn not asserted).
            // next CSn is asserted.
            if !tctx.inframe && tctx.state == TxState::Queued {
                tctx.state = TxState::Writing;
                spi.txerr_clear();
                spi.enable_tx();
                ringbuf_entry!(Trace::StartTx(ssa, ssd));
                //if ssa {
                    // There is work to do now.
                    // XXX YY again = true;
                //}
                // assert IRQ
                gpio.set_val(sp_irq, Value::Zero).unwrap_lite();
                tctx.rot_irq = true;
            }

            if tctx.state == TxState::Writing {
                loop {
                    if !spi.can_tx() {
                        break;
                    }
                    let b = tctx.tx[tctx.count];
                    ringbuf_entry!(Trace::Tx(tctx.state, b, tctx.count));
                    spi.send_u8(b);
                    tctx.count += 1;
                    if tctx.count == tctx.end {
                        tctx.state = TxState::Finish;
                        break;
                    }
                }
                if tctx.state == TxState::Writing {
                    // There are more bytes to transmit
                    again = true; // XXX this just causes a lot of looping.
                }
            }

            // TODO: check for tx underflow.
            // TODO: check for rx overflow.
            // TODO: Queue a failure message on those conditions.
            // TODO: Clear the error condition.

            // If we are asserting ROT_IRQ, check for conditions to de-assert.
            if tctx.rot_irq && tctx.state == TxState::Finish {
                let (_, _, _, txempty, _, _, _, _, _) = spi.stat();

                if txempty {
                    ringbuf_entry!(Trace::Line);
                    // Note: SP is going to be edge-triggered, so this signal
                    // does not need to be asserted for the entire transaction.
                    // However, it is nice to see it on the logic analyzer for
                    // the whole frame.
                    if gpio.set_val(sp_irq, Value::One).is_err() {
                        panic!();
                    }
                    tctx.rot_irq = false;
                    tctx.state = TxState::Idle;
                    ringbuf_entry!(Trace::DeassertRotIrq);
                    // If we have nothing to send in the next frame, then
                    // the LPC55 will send a byte of unknown origin.
                    spi.send_u8(0);     // write our no-op byte.
                    spi.drain_tx();     // Throw away byte just written.

                    // spi.disable_tx();   // Ignore Tx interrupts.
                    // spi.txerr_clear();
                } else {
                    // TODO: ssd interrupt would bring us back so we don't need
                    // to loop.
                    // As long as we are asserting ROT_IRQ, we will be trying
                    // to keep up with the SP clocking out the Tx FIFO or
                    // reacting to txempty.
                    // XXX YY again = true;
                }
            }

            // This is a slave SPI interface.
            // The master can always issue a command. It is not obligated
            // to read our response though one will always be available.
            // The master might restart or othewise become out of sync at any
            // time. It may want to start a new session arbitrarily.
            //
            // Rx is always enabled at the start of CSn asserted (sot).
            // Rx will always be read to keep FIFO management simple.
            // If the first byte indicates a valid protocol, it will be
            // processed. Protocol errors are reported if they occur.
            // A 0x00 byte at start will ignore bytes until next start.
            // A non-zero, non-supported protocol will have an error response.
            //
            // If we received the ssd condition, then the read FIFO may still have
            // several bytes left. Get them now because it is then time to
            // process the received message.
            loop {
                if !spi.has_byte() {
                    break;
                }
                let (b, csn, sot) = spi.read_u8_csn_sot(); // always read
                ringbuf_entry!(Trace::Rx(rctx.state, b, rctx.count, csn, sot));
                rctx.rx_byte(b, sot, &mut tctx);
            }

            // CSn de-assert is detected to catch the end of frame condition.
            // End of frame disagreement with count from header is cause
            // for sending an error response.
            if ssd {
                ringbuf_entry!(Trace::EndOfFrame);
                // The SP is not talking to us, it cannot be consuming data
                // If we were recieving and didn't get a complete message,
                // then we're not getting any more bytes and that could be
                // an error condition.
                // If we were transmitting, then transmit cannot continue.

                // partial header or payload received
                if rctx.state == RxState::Header ||
                    rctx.state == RxState::Payload {
                        // XXX include a more useful error response?
                        ringbuf_entry!(Trace::RxFragment);
                        if tctx.enqueue(MsgType::Error, None) {
                            ringbuf_entry!(Trace::CannotSendRxFragError);
                            rctx.state = RxState::Dispatch;
                            // XXX YY again = true;
                        }
                }
                // Process transitory states
                if rctx.state == RxState::Dispatch {
                    // Receive is complete.
                    // Processing may take a long time and detract from keeping up
                    // with Tx and Rx FIFOs.
                    // Processing has been delayed until CSn was de-asserted.
                    rctx.state = RxState::Responding;
                    ringbuf_entry!(Trace::Responding);

                    if tctx.state == TxState::Idle {
                        let rmsg = Msg::parse(&mut *rctx.rx).unwrap_lite();

                        // Errors and SP always being allowed to send a command
                        // on CSn assert means that there can be corner cases.
                        // XXX Is the Tx FIFO empty or is the result of a previous
                        // command still queued?
                        // XXX Is CSn currently asserted?
                        // XXX Is SP_IRQ still asserted?

                        // Invalid message protocol and length are already
                        // handled in rx_byte().
                        match rmsg.msgtype() {
                            MsgType::Echo => {
                                ringbuf_entry!(Trace::RespondToEcho);
                                tctx.enqueue(MsgType::EchoReturn,
                                    rmsg.payload_get().ok());
                                again = true;
                            }
                            MsgType::Sprockets => {
                                // TODO: Replace with actual sprockets handling.
                                ringbuf_entry!(Trace::RespondToSprockets);
                                tctx.enqueue(MsgType::Sprockets,
                                    rmsg.payload_get().ok());
                            },
                            _ => {
                                // The message received was an unknown type.
                                ringbuf_entry!(Trace::RespondToUnknown);
                                tctx.enqueue(MsgType::Error, None);
                            }
                        }
                    }
                }
            }
            if !again {
                break;
            }
        }
        sys_irq_control(1, true);
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
