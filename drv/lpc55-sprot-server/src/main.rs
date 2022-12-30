// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the LPC55 HighSpeed SPI interface.
//!
//! See drv/sprot-api/README.md
//! Messages are received from the Service Processor (SP) over a SPI interface.
//!
//! The RoT indicates that a response is ready by asserting ROT_IRQ to the SP.
//!
//! The protocol implemented is strictly request/response. While an RoT is
//! responding to an SP request, the SP should not be sending another messsage
//! This drastically simplifies the state machine and helps us easily catch
//! when the SP is moving too fast for the RoT to catch up.
//!
//! See drv/sprot-api for message layout.
//!
//! If the payload length exceeds the maximum size or not all bytes are received
//! before CSn is de-asserted, the message is malformed and an ErrorRsp message
//! will be sent to the SP.
//!
//! Messages from the SP are not processed until the SPI chip-select signal
//! is deasserted.
//!
//! ROT_IRQ is intended to be an edge triggered interrupt on the SP.
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
    IoStats, MsgHeader, Protocol, RxMsg, SprotError, TxMsg, VerifiedTxMsg,
    BUF_SIZE, HEADER_SIZE,
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
    IgnoreOnParse,
    State(State),
    Stats(IoStats),
    PrimingWithZero,
    Intstat(u32),
    Stat(u32),
    FifoStat(u8, u8, bool, bool),
    CsnDeasserted,
    Bytes(usize, usize, usize),
}
ringbuf!(Trace, 128, Trace::None);

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);

// Notification mask for Flexcomm8 hs_spi IRQ; must match config in app.toml
const SPI_IRQ: u32 = 1;

/// Setup spi and its associated GPIO pins
fn configure_spi() -> Io {
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

    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_driver);

    Io {
        spi,
        gpio,
        state: State::init(),
        stats: IoStats::default(),
    }
}

// Container for spi and gpio
struct Io {
    spi: crate::spi_core::Spi,
    gpio: drv_lpc55_gpio_api::Pins,
    state: State,
    stats: IoStats,
}

/// The state of the IO state machine
///
/// Logically, the sprot protocol operates in a request/response fashion,
/// despite the fact that bytes must simultaneously be clocked in and out during SPI
/// transactions.
///
/// As part of our logical state machine, the RoT can only be reading a request from
/// the SP or writing a reply. There can only be one request in flight at a time.
/// When desynchronization is detected it must be corrected.
///
/// The RoT always starts in State::Read(ReadState::WaitingForRequest);
#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Read(ReadState),
    Write(WriteState),
}

impl State {
    /// Initialize the state Io state machine
    fn init() -> State {
        State::Read(ReadState::WaitingForRequest)
    }

    /// Return the read state. Panics if the RoT is not currently reading.
    fn read(&self) -> ReadState {
        match self {
            State::Read(s) => *s,
            _ => panic!(),
        }
    }

    /// Return the write state. Panics if the RoT is not currently writing.
    fn write(&self) -> WriteState {
        match self {
            State::Write(s) => *s,
            _ => panic!(),
        }
    }
}

/// The state of the IO state machine when trying to read a request from the SP
#[derive(Clone, Copy, PartialEq)]
pub enum ReadState {
    /// The RoT is waiting for CSn to be asserted
    WaitingForRequest,

    /// The RoT is in frame and reading data until it sees CSn de-asserted
    InFrame,

    /// The frame was read cleanly.
    ///
    /// In this case we were:
    ///   1. In `WaitingForRequest` and saw CSn asserted and CSn de-asserted at the
    ///      same time with no overrun errors, and with data.
    ///   2. In `InFrame` and saw CSn de-asserted with no overrun errors and
    ///     with data.
    ///
    FrameRead,

    /// The SP has pulsed CSn, which means that CSn was asserted and de-
    /// asserted with no data clocked at all.
    ///
    /// The SP pulses CSn when it receives a ROT_IRQ it wasn't expecting. This
    /// happens in the reply phase, and can occur if processing a request at
    /// the RoT takes longer than a retry timeout at the SP.
    ///
    /// We can only transition to Flush in the `WaitingForRequest` or `InFrame`
    /// states. Flush is a `transient` state indicating we should clear our
    /// FIFOs and buffers and go back to `WaitingForRequest`.
    Flush,

    /// We missed reading some bytes. We should clear our buffers and fifos and
    /// inform the SP.
    Overrun,

    /// We got an unexpected CSn Assert. This is either due to a weird timing
    /// issue resulting in the next request being sent, or a CSn pulse.
    ///
    /// Either way we are desynchronized, and the safest thing to do is
    /// to clear our buffers and fifos and go back to `WaitingForRequest`
    /// *without* sending a reply. We do *NOT* want to send a reply for the
    /// following three reasons:
    ///
    /// 1. The CSn assert is the start of a CSn pulse. Sending a reply triggers
    /// an ROT_IRQ assert which is exactly what the CSn pulse is trying to get
    /// rid of!
    ///
    /// 2. The CSn assert is the start of a new request and the new request
    /// fits in one fifo. In this case the response from the prior request
    /// will be interpreted as the response for the new request. While we
    /// could use sequence numbers to clear up this ambiguity, doing so adds
    /// complexity.
    ///
    /// 3. The CSn assert is the start of a new request and the new request
    /// does not fit in one fifo. We will go process the prior response, and
    /// almost certainly end up with an overrun while doing so, confusing
    /// the SP with the same ambiguity as case 2 above, and then sending an
    /// additonal error response for the overrun. The additional error response
    /// may come during  another request, since the SP thinks the one we are
    /// sending the overrun error for has already completed. This will cause
    /// the SP to see an unexpected ROT_IRQ and trigger a CSn pulse. Even if
    /// there is no overrun, the reply from the prior request will cause the
    /// SP to either see that the current request is complete, (if its data
    /// fit in only 2 fifos), or to to get a reply while it's still trying to
    /// send the current request. In either of these cases, the SP will see an
    /// unexpected ROT_IRQ and perform a CSn pulse.
    ///
    /// Note that it is possible that we got an overrun error and an unexpected
    /// CSn Assert. In this case, the unexpected CSn assert takes behavioral
    /// precedance. We do not want to send a reply for all the reasons listed
    /// above.
    UnexpectedCsnAssert,

    /// The SP sent us more bytes than `BUF_SIZE`. This is a serious error.
    /// We must inform the SP.
    SpProtocolCapabilitiesMismatch,
}

/// The state of the IO state machine when trying to write a response to the SP
#[derive(Clone, Copy, PartialEq)]
pub enum WriteState {
    /// The RoT has asserted ROT_IRQ, and is waiting for CSn to be asserted
    /// so it can clock out it's response.
    WaitingToTransmit,

    /// The RoT is writing it's response
    Writing,

    /// The response has successfully been written out, ROT_IRQ has been
    /// de-asserted, and CSn has been de-asserted. We can safely go back to
    /// reading.
    ResponseWritten,

    /// The SP has pulsed CSn, which means that CSn was asserted and de-
    /// asserted with no data clocked at all. This means that the response
    /// from the RoT was unexpected.
    Flush,

    /// The RoT did not clock out all its response bytes in time.
    /// The RoT must de-assert ROT_IRQ, wait for CSn to be de-asserted,
    /// and then go back to waiting for the next request.
    Underrun,
}

#[export_name = "main"]
fn main() -> ! {
    let mut io = configure_spi();

    let mut rx_buf = [0u8; BUF_SIZE];
    let mut tx_buf = [0u8; BUF_SIZE];

    let mut handler = Handler::new();

    loop {
        let rx_msg = RxMsg::new(&mut rx_buf[..]);
        let tx_msg = TxMsg::new(&mut tx_buf[..]);
        if let Some(response) = io.spi_read(rx_msg, tx_msg, &mut handler) {
            ringbuf_entry!(Trace::Stats(io.stats));
            io.spi_write(response);
            ringbuf_entry!(Trace::Stats(io.stats));
        }
    }
}

impl Io {
    /// Put a Protocol::Busy value in FIFOWR so that SP/logic analyzer knows
    /// we're away.
    ///
    /// This is primarily for debugging via logic analyzer, as it's only done
    /// after reading a request but before sending a response. The SP won't
    /// process a response unless an ROT_IRQ is raised, so it will never notice
    /// this byte. That is actually what we want as the rest of the handling is
    /// done correctly on the RoT side. If the SP actually reacted to this byte
    /// then it would alter its behavior to deal with it, but the only way it
    /// would see it is if it already clocked a CSn assert. Once that occurs,
    /// the RoT will notice an overrun and report it to the SP anyway. No need
    /// to pulse or do anything special here.
    ///
    /// At the same time, it's nice to see this byte on the logic analyzer to
    /// indicate what's going on.
    fn mark_busy(&mut self) {
        self.spi.drain_tx();
        self.spi.send_u8(Protocol::Busy as u8);
    }

    /// Write a response to the SP
    ///
    /// We expect to only clock in 0s from the SP, or else the SP and RoT are
    /// desynchronized.
    pub fn spi_write<'a>(&mut self, response: VerifiedTxMsg) {
        self.state = State::Write(WriteState::WaitingToTransmit);

        // Prime the fifo with the first part of the response to prevent
        // underrun while waiting for an interrupt.
        self.spi.drain_tx();
        let mut tx_iter = response.iter();
        while self.spi.can_tx() {
            if let Some(b) = tx_iter.next() {
                self.spi.send_u8(b);
            } else {
                self.spi.send_u8(0);
            }
        }
        let fifostat = self.spi.fifostat();
        ringbuf_entry!(Trace::FifoStat(
            fifostat.txlvl().bits(),
            fifostat.rxlvl().bits(),
            fifostat.txerr().bits(),
            fifostat.rxerr().bits()
        ));

        self.assert_rot_irq();
        loop {
            sys_irq_control(SPI_IRQ, true);
            sys_recv_closed(&mut [], SPI_IRQ, TaskId::KERNEL).unwrap_lite();
            loop {
                ringbuf_entry!(Trace::State(self.state));
                match self.state.write() {
                    WriteState::WaitingToTransmit => {
                        self.write_wait_for_csn_asserted();
                        if self.state
                            == State::Write(WriteState::WaitingToTransmit)
                        {
                            // wait for an interrupt
                            break;
                        }
                    }
                    WriteState::Writing => self.write_response(&mut tx_iter),
                    WriteState::ResponseWritten
                    | WriteState::Flush
                    | WriteState::Underrun => {
                        return;
                    }
                }
            }
        }
    }

    /// Read a request from the SP
    ///
    /// We clock out 0s until CSn is de-asserted
    pub fn spi_read<'a>(
        &mut self,
        mut rx_msg: RxMsg<'a>,
        tx_msg: TxMsg<'a>,
        handler: &mut Handler,
    ) -> Option<VerifiedTxMsg<'a>> {
        self.state = State::Read(ReadState::WaitingForRequest);
        self.zero_tx_buf();
        loop {
            sys_irq_control(SPI_IRQ, true);
            sys_recv_closed(&mut [], SPI_IRQ, TaskId::KERNEL).unwrap_lite();
            loop {
                ringbuf_entry!(Trace::State(self.state));
                match self.state.read() {
                    ReadState::WaitingForRequest => {
                        self.read_wait_for_csn_asserted();
                        if self.state
                            == State::Read(ReadState::WaitingForRequest)
                        {
                            // Wait for interrupt
                            break;
                        }
                    }
                    ReadState::InFrame => {
                        self.read_until_csn_deasserted(&mut rx_msg)
                    }
                    ReadState::FrameRead => {
                        self.mark_busy();
                        return handler.handle(rx_msg, tx_msg, &mut self.stats);
                    }
                    ReadState::Flush | ReadState::UnexpectedCsnAssert => {
                        return None;
                    }
                    ReadState::Overrun => {
                        self.mark_busy();
                        return Some(handler.flow_error(tx_msg));
                    }
                    ReadState::SpProtocolCapabilitiesMismatch => {
                        self.mark_busy();
                        return Some(handler.protocol_error(tx_msg));
                    }
                }
            }
        }
    }

    // We are waiting for CSn to be asserted so we can transmit our response.
    fn write_wait_for_csn_asserted(&mut self) {
        // Get frame start/end interrupt from intstat (SSA/SSD).
        let intstat = self.spi.intstat();
        ringbuf_entry!(Trace::Intstat(intstat.bits()));

        // CSn asserted by the SP.
        if intstat.ssa().bit() {
            self.spi.ssa_clear();
            self.state = State::Write(WriteState::Writing);
        }
    }

    fn write_response(&mut self, tx_iter: &mut dyn Iterator<Item = u8>) {
        let mut bytes_read: usize = 0;
        let mut data_bytes_written: usize = 0;
        let mut zeros_written: usize = 0;
        let mut state = self.state.write();
        let mut overrun_seen = false;

        // Set to true when `tx_iter` has been exhausted. Note that we may
        // continue sending `0` after this if we are still clocking in bytes
        // from the SP.
        let mut response_sent = false;

        loop {
            let intstat = self.spi.intstat();
            let fifostat = self.spi.fifostat();

            // We check for CSn deasserted at the beginning of the loop because
            // we always want an opportunity drain our fifos after we see the bit set.
            let csn_deasserted = self.spi.ssd();

            if fifostat.txerr().bit() {
                // We missed our window to send :(
                self.spi.txerr_clear();
                self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
                // If we've already written our full response, underrun doesn't matter.
                // This can happen if we clock out BUF_SIZE of data, but are waiting
                // for a CSn de-assert.
                if state != WriteState::ResponseWritten {
                    state = WriteState::Underrun;
                }
            }

            if fifostat.rxerr().bit() {
                // We missed a byte to read. But we are only expecting 0s, so
                // this isn't critical, except to judge if we got a CSn pulse
                // or not.
                self.spi.rxerr_clear();
                self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
                overrun_seen = true;
            }

            let mut io = true;
            while io == true {
                io = false;
                if self.spi.can_tx() {
                    io = true;
                    if let Some(b) = tx_iter.next() {
                        data_bytes_written += 1;
                        self.spi.send_u8(b);
                    } else {
                        response_sent = true;
                        // Just clock out zeros and prevent an unnecessary underrun
                        self.spi.send_u8(0);
                        zeros_written += 1;
                    }
                }

                if self.spi.has_byte() {
                    io = true;
                    // The SP should only clock out zeros while waiting for a
                    // response. We'll record it, but still finish sending our reply.
                    if self.spi.read_u8() != 0 {
                        self.stats.received_data_while_replying = self
                            .stats
                            .received_data_while_replying
                            .wrapping_add(1);
                    }
                    bytes_read += 1;
                }
            }

            // Are we done sending our response?
            if response_sent && state == WriteState::Writing {
                state = WriteState::ResponseWritten;
            }

            // Check for a CSn de-assert
            if csn_deasserted {
                ringbuf_entry!(Trace::CsnDeasserted);
                self.spi.ssd_clear();

                // Is this a CSn pulse?
                if bytes_read == 0
                    && state == WriteState::Writing
                    && !overrun_seen
                {
                    self.stats.csn_pulses =
                        self.stats.csn_pulses.wrapping_add(1);
                    state = WriteState::Flush;
                    break;
                }

                if state == WriteState::ResponseWritten {
                    // We're done!
                    break;
                }

                // Just go ahead and flush if we are still writing
                if state == WriteState::Writing {
                    // Uh-oh, we didn't actually complete writing our response.
                    self.stats.unexpected_csn_deassert_while_replying = self
                        .stats
                        .unexpected_csn_deassert_while_replying
                        .wrapping_add(1);
                    state = WriteState::Flush;
                }
                break;
            }
        }

        self.deassert_rot_irq();
        self.state = State::Write(state);
    }

    // We are waiting for a new request from the SP.
    fn read_wait_for_csn_asserted(&mut self) {
        // Get frame start/end interrupt from intstat (SSA/SSD).
        let intstat = self.spi.intstat();
        ringbuf_entry!(Trace::Intstat(intstat.bits()));

        // CSn asserted by the SP.
        if intstat.ssa().bit() {
            self.spi.ssa_clear();
            self.state = State::Read(ReadState::InFrame);
        }
    }

    // Read data in a tight loop until we see CSn de-asserted
    //
    // XXX Denial of service by forever asserting CSn?
    // We could mitigate by imposing a time limit
    // and resetting the SP if it is exceeded.
    // But, the management plane is going to notice that
    // the RoT is not available. So, does it matter?
    fn read_until_csn_deasserted<'a>(&mut self, rx_msg: &mut RxMsg<'a>) {
        let mut num_unexpected_csn_asserts_in_this_loop: u32 = 0;
        let mut overrun_seen = false;
        let mut state = self.state.read();
        loop {
            let intstat = self.spi.intstat();
            let fifostat = self.spi.fifostat();
            let mut csn_deasserted = self.spi.ssd();

            if csn_deasserted {
                // Cool, we have a complete frame.
                self.spi.ssd_clear();

                // We don't want to overwrite any error states
                if state == ReadState::InFrame {
                    state = ReadState::FrameRead;
                }
            }

            // Let's check for any problems

            if fifostat.txerr().bit() {
                // We don't do anything with tx errors other than record them
                self.spi.txerr_clear();
                self.stats.tx_underrun = self.stats.tx_underrun.wrapping_add(1);
            }

            if intstat.ssa().bit() {
                self.spi.ssa_clear();
                state = ReadState::UnexpectedCsnAssert;

                // We have to keep pulling bytes, waiting for the next CSnDeassert
                // Let's also keep track of this.
                csn_deasserted = false;
                self.stats.unexpected_csn_asserts =
                    self.stats.unexpected_csn_asserts.wrapping_add(1);
                num_unexpected_csn_asserts_in_this_loop += 1;
                if num_unexpected_csn_asserts_in_this_loop
                    > self.stats.max_unexpected_csn_asserts_in_one_read_loop
                {
                    self.stats.max_unexpected_csn_asserts_in_one_read_loop =
                        num_unexpected_csn_asserts_in_this_loop;
                }
            }

            if fifostat.rxerr().bit() {
                // Rx errors are more important. They mean we're missing
                // data. We should report this to the SP. This can be used to
                // potentially throttle sends in the future.
                self.spi.rxerr_clear();
                self.stats.rx_overrun = self.stats.rx_overrun.wrapping_add(1);
                overrun_seen = true;

                // Other error states take precedence
                if state == ReadState::InFrame {
                    state = ReadState::Overrun;
                }
            }

            let mut io = true;
            while io == true {
                io = false;
                if self.spi.has_byte() {
                    io = true;
                    let b = self.spi.read_u8();
                    if rx_msg.push(b).is_err()
                        && state != ReadState::UnexpectedCsnAssert
                    {
                        // The SP has sent us more then BUF_SIZE bytes! This is
                        // a major problem. Either we somehow got desynchronized
                        // or the SP is confused about our capabilities. If there
                        // is an overrun, we're likely desynchronized. However, an
                        // overrun should give us fewer bytes to receive, not more
                        // so the SP must be confused about our buffer size unless
                        // it's already asserted CSn for the next message.
                        //
                        // If we are desynchronized due to receiving an unexpected
                        // CSn Assert, we'll also see that in our check below, and
                        // behave as we normally do in that case.
                        //
                        // If we don't have an untimely CSn Assert then the SP
                        // is confused. We should keep pulling bytes until CSn
                        // de-assert to stay synchronized so the SP can handler
                        // replies, and then inform the SP of the problem.
                        self.stats.rx_protocol_error_too_many_bytes = self
                            .stats
                            .rx_protocol_error_too_many_bytes
                            .wrapping_add(1);

                        state = ReadState::SpProtocolCapabilitiesMismatch;
                    }
                }
                if self.spi.can_tx() {
                    io = true;
                    self.spi.send_u8(0);
                }
            }

            if csn_deasserted {
                // We need to break out of this loop at some point!
                //
                // We could check again to see if CSn is asserted, but
                // logically we know that this CSn deassert *happened
                // before* any new CSn asserts. So we should move on as normal
                // and let the next read handle the CSn assert. Seeing another
                // CSn assert so quickly, while in our tight loop would mean
                // there is a major problem in our protocol. If it happens
                // we'll also detect if via an Overrun on the next message most
                // likely, unless it's a CSn pulse.
                break;
            }
        }

        // If we received 0 bytes and we never saw an overrun, then this was a
        // CSn pulse. A CSn pulse can happen at *any time* due to an SP restart
        // or other error. In this case, any unexpected CSn assert was the
        // start of a pulse. While the behavior of both those states is the
        // same, for tracking purposes we return in `ReadState::Flush`, since
        // that's more specific, and not an error.
        if rx_msg.len() == 0 && !overrun_seen {
            self.stats.csn_pulses = self.stats.csn_pulses.wrapping_add(1);
            state = ReadState::Flush;
        }

        self.state = State::Read(state);
    }

    // When waiting for a request from the SP, we want to have the SPI
    // controller on the SP clock in zeros while it's transmitting to us.
    // We therefore prime the fifo with 8 zeros.
    // According the lpc55 manual from NXP (section 35.2) there are 8 entries in each fifo.
    // This has been shown with ringbuf tracing as well.
    fn zero_tx_buf(&mut self) {
        self.spi.drain_tx(); // FIFOWR is now empty; we'll get an interrupt.
        while self.spi.can_tx() {
            self.spi.send_u8(0);
        }
    }

    fn assert_rot_irq(&self) {
        self.gpio.set_val(ROT_IRQ, Value::Zero).unwrap_lite();
    }

    fn deassert_rot_irq(&self) {
        self.gpio.set_val(ROT_IRQ, Value::One).unwrap_lite();
    }
}

fn turn_on_flexcomm(syscon: &Syscon) {
    // HSLSPI = High Speed Spi = Flexcomm 8
    // The L stands for Let this just be named consistently for once
    syscon.enable_clock(Peripheral::HsLspi).unwrap_lite();
    syscon.leave_reset(Peripheral::HsLspi).unwrap_lite();
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
