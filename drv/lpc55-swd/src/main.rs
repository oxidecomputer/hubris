// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// SWD (Serial Wire Debug) control from the RoT to the SP
//
// The ARM Debug Interface Specification (ADI) describes the Serial Wire Debug
// Port (SW-DP) protocol. This is frequently implemented as a bit-bang protocol
// (setting the GPIO pins manually). Per 5.3.2 of ADIv5
//
// A successful write operation consists of three phases:
// - an eight-bit write packet request, from the host to the target
// - a three-bit OK acknowledge response, from the target to the host
// - a 33-bit data write phase, from the host to the target.
//
// There is one bit of turnaround between the request and acknowledge and
// the acknowledge and write phase.
//
// Per 5.3.3 of ADIv5
//
// A successful read operation consists of three phases:
// - an eight-bit read packet request, from the host to the target
// - a three-bit OK acknowledge response, from the target to the host
// - a 33-bit data read phase, where data is transferred from the target to the
//   host
//
// There is one bit of turnaround between the request and acknowledge.
//
// It turns out this specification can be implemented on top of a SPI block
// with some specific implementation details:
//
// - SPI has 4 pins (MOSI, MISO, CS, CSK) and importantly expects MOSI and MISO
//   to be separate. This is in contrast to SWD which assumes a single pin for
//   both input and output. For our SPI implementation, we tie MOSI and MISO
//   together and configure exactly one of MOSI or MISO for use with SPI at a
//   time.
//
//   A side effect of this choice is that reading needs to be precise with the
//   specification. Adding extra idle cycles is fairly easy when writing but
//   that is not possible with reading.
//
// - The LPC55 can transmit between 4 and 16 bits at a time. This makes
//   makes transmitting the various combinations of phases and turnaround bits
//   a bit of a pain. This is broken up into the following combinations:
//
//   -- 8-bit packet write
//   -- 4-bit ACK read (turnaround + three bits of response)
//   -- 34-bit write (one bit turnaround, 32 bits data, one bit parity) broken
//      up into 9 bit + 8 bit + 8 bit + 9 bit writes
//   -- 33-bit read (32 bits data, one bit parity) broken up into 8 bit +
//      8 bit + 8 bit + 9 bit reads. There is also one bit of turnaround after
//      the read but this is absorbed into idle cycles.
//
// - The SWD protocol is LSB first. This works very well when bit-banging but
//   somewhat less well with a register based hardware block such as SPI. The
//   SPI controller can do LSB first transfers but it turns out to be easier to
//   debug and understand if we keep it in MSB form and reverse bits where
//   needed. Endianness is one of the hardest problems in programming after
//   all.

// SWD functions and clients:
//
// The `swd` task is currently used for three different purposes.
//   - Measurement of the active SP flash bank on detection of SP_RESET,
//   - Update watchdog for the SP Hubris image,
//   - SP task dump
//
// Measurement: SP measurement is relatively atomic with respect to the
// other work in that it is done entirely within the interrupt notification
// handler. Note that the SP_RESET handler will change the SP_RESET pin
// from input to output and back again. By design, there is no code path
// out of that handler that leaves the SP_RESET pin as an output. If the
// RoT was to itself reset, or the swd task to restart, during SP_RESET
// handling, the `setup_pins` call in main would return SP_RESET to its
// proper configuration.
//
// Watchdog: The watchdog timer is intended to work across SP_RESET.
// Although it checks that the SWD interface can be used when the timer
// is first set, it is not until the timer fires that the SP SWD
// interface is used.
//
// Dumper: There is a potential conflict where `dumper` is actively using
// the `swd` task and the watchdog timer fires, or SP_RESET or JTAG_DETECT
// interrupts are received. Any of these mean that either the dump information
// has been erased in the SP or the SP is no longer accessible via SWD. While
// these scenarios are highly unlikely, all that is needed is to force any
// non-idempotent API call to initialize the SWD interface before use.
// The dumper task may fail, but that is appropriate.

#![no_std]
#![no_main]

use attest_api::{Attest, AttestError, HashAlgorithm};
use drv_lpc55_gpio_api::{Direction, Pins, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_sp_ctrl_api::SpCtrlError;
use endoscope_abi::{Shared, State};
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use lpc55_pac as device;
use ringbuf::*;
use static_assertions::const_assert;
use userlib::{
    hl, set_timer_relative, sys_get_timer, sys_irq_control,
    sys_irq_control_clear_pending, sys_set_timer, task_slot, FromPrimitive,
    RecvMessage, TaskId, UnwrapLite,
};
use zerocopy::IntoBytes;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Start {
        is_dongle_connected: bool,
    },
    Idcode(u32),
    Idr(u32),
    MemVal(u32),
    ReadCmd,
    WriteCmd,
    AckErr(Ack),
    DongleDetected,
    Dhcsr(Dhcsr),
    ParityFail {
        data: u32,
        received_parity: u16,
    },
    EnabledWatchdog,
    DisabledWatchdog,
    WatchdogFired,
    WatchdogSwap(Result<(), Ack>),
    AttestError(AttestError),
    BadLen,
    CannotWriteVtor,
    Data {
        addr: u32,
        data: u32,
        src: u32,
    },
    DemcrWriteError,
    DfsrReadError,
    Dfsr(Dfsr),
    DhcsrWriteError,
    DidNotHalt {
        sp_reset_state: Value,
    },
    DoHalt,
    DoSetup,
    EndOfNotificationHandler,
    Halted {
        delta_t: u32,
    },
    HaltFail(u32),
    Injected {
        start: u32,
        length: usize,
        delta_t: u32,
    },
    InjectionFailed,
    InvalidatedSpMeasurement,
    InvalidateSpMeasurement,
    LimitRemaining(u32),
    MeasuredSp {
        success: bool,
        delta_t: u32,
    },
    MeasureFailed,
    SharedState(u32),
    Digest(usize, [u8; 8]),
    ReadbackFailure,
    ReadBufFail,
    ReadX {
        start: u32,
        len: u32,
    },
    RecordedMeasurement,
    RecordMeasurementFailed,
    Resumed,
    ResumeFail,
    SetupSwdOk,
    SpJtagDetectFired,
    SpResetAsserted,
    SpResetFired,
    SpResetNotAsserted,
    SwdSetupFail,
    SwdSetupOk,
    TimerHandlerError(SpCtrlError),
    VcCoreResetNotCaught,
    WaitingForSpHalt {
        timeout: u32,
    },
    WrotePcRegisterFail,
    WroteSpRegisterFail,
    TokenWriteFail(Ack),
}

ringbuf!(Trace, 128, Trace::None);

task_slot!(SYSCON, syscon_driver);
task_slot!(GPIO, gpio_driver);
task_slot!(ATTEST, attest);

#[derive(Copy, Clone, PartialEq)]
enum Ack {
    //Ok,
    Wait,
    Fault,
    Protocol,
}

impl From<Ack> for SpCtrlError {
    fn from(val: Ack) -> Self {
        match val {
            Ack::Wait => SpCtrlError::AckWait,
            Ack::Fault => SpCtrlError::AckFault,
            Ack::Protocol => SpCtrlError::AckProtocol,
        }
    }
}

impl From<Ack> for RequestError<SpCtrlError> {
    fn from(val: Ack) -> Self {
        let e: SpCtrlError = val.into();
        e.into()
    }
}

// ADIv5 11.2.1 describes the CSW bits. Several of those fields (DbgSwEnable,
// Prot, SPIDEN) are implementation defined. RM0433 60.4.2 gives us the details
// of the implementation we care about. Note that the "Cacheable" bit (bit 27)
// is essential to correctly read memory that is in fact dirty in the L1 and
// has not been written back to SRAM!

// Full 32-bit word transfer
const CSW_SIZE32: u32 = 0x00000002;
// Increment by size bytes in the transaction
const CSW_SADDRINC: u32 = 0x00000010;
// AP access enabled
const CSW_DBGSTAT: u32 = 0x00000040;
// Cacheable + privileged + data access
// const CSW_HPROT: u32 = 0x0b << 24;
const CSW_HPROT: u32 = CSW_HPROT_0INSTRUCTION_FETCH_1DATA_ACCESS
    | CSW_HPROT_0MODE_USER_1PRIVILEGED
    | CSW_HPROT_0NONCACHE_1CACHEABLE;

const _CSW_SPROT: u32 = 1 << 30;

const CSW_HPROT_0INSTRUCTION_FETCH_1DATA_ACCESS: u32 = 0b00001 << 24;
const CSW_HPROT_0MODE_USER_1PRIVILEGED: u32 = 0b00010 << 24;
const _CSW_HPROT_0NONBUF_1BUFFERABLE: u32 = 0b00100 << 24;
const CSW_HPROT_0NONCACHE_1CACHEABLE: u32 = 0b01000 << 24;
const _CSW_HPROT_0NONEXCL_1EXCLUSIVE: u32 = 0b10000 << 24;

const DP_CTRL_CDBGPWRUPREQ: u32 = 1 << 28;
const DP_CTRL_CDBGPWRUPACK: u32 = 1 << 29;

// See Ch5 of ARM ADI for bit pattern
const START_BIT: u8 = 7;
// Stop is bit 1 and always 0
const PARITY_BIT: u8 = 2;
const ADDR_BITS: u8 = 3;

const RDWR_BIT: u8 = 5;
const APDP_BIT: u8 = 6;
const PARK_BIT: u8 = 0;

const START_VAL: u8 = 1 << START_BIT;
const PARK_VAL: u8 = 1 << PARK_BIT;

// In most cases, `swd` using the DP to request a halt will complete
// in under 1ms. When injecting code into the SP and waiting for the
// SP flash bank measurement to complete (and halt), it will take
// about 250ms. And lastly, if a measurement is initiated by having a
// human press a physical reset button, the time to SP_RESET
// de-assertion is not bounded.
const WAIT_FOR_HALT_MS: u64 = 500;

// Debug Interface from Armv7 Architecture Manual chapter C-1
mod armv7debug;

use armv7debug::{Demcr, Dfsr, Dhcsr, DpAddressable, Reg, DCRDR, DCRSR, VTOR};

#[derive(Copy, Clone, PartialEq)]
enum Port {
    DP = 0,
    AP = 1,
}

#[repr(u8)]
#[derive(Copy, Clone, PartialEq)]
enum DpRead {
    IDCode = 0x0,
    Ctrl = 0x4,
    //Resend = 0x8,
    Rdbuf = 0xc,
}

impl DpRead {
    fn addr_bits(&self) -> u8 {
        // Everything in SWD is transmitted LSB first.
        // This represents bits [2:3] of the address in the form we want
        // to transfer.
        match *self {
            DpRead::IDCode => 0b00,
            DpRead::Ctrl => 0b10,
            DpRead::Rdbuf => 0b11,
        }
    }
}

#[repr(u8)]
#[derive(Copy, Clone, PartialEq)]
enum DpWrite {
    Abort = 0x0,
    Ctrl = 0x4,
    Select = 0x8,
}

impl DpWrite {
    fn addr_bits(&self) -> u8 {
        // Everything in SWD is transmitted LSB first.
        // This represents bits [2:3] of the address in the form we want
        // to transfer.
        match *self {
            DpWrite::Abort => 0b00,
            DpWrite::Ctrl => 0b10,
            DpWrite::Select => 0b01,
        }
    }
}

#[repr(u8)]
#[derive(Copy, Clone, PartialEq)]
enum RawSwdReg {
    DpRead(DpRead),
    DpWrite(DpWrite),
    ApRead(ApReg),
    ApWrite(ApReg),
}

#[repr(u8)]
#[derive(Copy, Clone, PartialEq)]
// Be picky and match the spec
#[allow(clippy::upper_case_acronyms)]
enum ApReg {
    CSW = 0x0,
    TAR = 0x4,
    DRW = 0xC,
    //BD0 = 0x10,
    //BD1 = 0x14,
    //BD2 = 0x18,
    //BD3 = 0x1C,
    //ROM = 0xF8,
    IDR = 0xFC,
}

impl ApReg {
    fn addr_bits(&self) -> u8 {
        // Everything in SWD is transmitted LSB first.
        // This represents bits [2:3] of the address in the form we want
        // to transfer.
        match *self {
            ApReg::CSW => 0b00,
            ApReg::TAR => 0b10,
            ApReg::DRW => 0b11,

            ApReg::IDR => 0b11,
        }
    }
}

// represents the port + register
struct ApAddr(u32, ApReg);

fn get_addr_and_rw(reg: RawSwdReg) -> (u8, u8) {
    match reg {
        RawSwdReg::DpRead(v) => (1 << RDWR_BIT, v.addr_bits() << ADDR_BITS),
        RawSwdReg::DpWrite(v) => (0 << RDWR_BIT, v.addr_bits() << ADDR_BITS),
        RawSwdReg::ApRead(v) => (1 << RDWR_BIT, v.addr_bits() << ADDR_BITS),
        RawSwdReg::ApWrite(v) => (0 << RDWR_BIT, v.addr_bits() << ADDR_BITS),
    }
}

// The parity is only over 4 of the bits
fn calc_parity(val: u8) -> u8 {
    let b = val >> 3 & 0xf;

    ((b.count_ones() % 2) as u8) << PARITY_BIT
}

#[derive(Copy, Clone, PartialEq)]
struct MemTransaction {
    total_word_cnt: usize,
    read_cnt: usize,
}

// Internal to setup
enum SwdSetupErr {
    IdCode(Ack),
    PowerUp(Ack),
    Idr(Ack),
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum SwdState {
    /// The SWD GPIOs are configured as inputs
    ///
    /// This is used when a physical debugger is attached to the SP (detected
    /// through the JTAG_DETECT line)
    Disconnected,

    /// The SWD GPIOs are configured
    Connected {
        /// Marks whether SWD has been initialized
        init: bool,
    },
}

struct ServerImpl {
    spi: spi_core::Spi,
    gpio_task: TaskId,
    gpio: drv_lpc55_gpio_api::Pins,
    attest: Attest,
    state: SwdState,
    transaction: Option<MemTransaction>,
}

impl idl::InOrderSpCtrlImpl for ServerImpl {
    fn read_transaction_start(
        &mut self,
        _: &RecvMessage,
        start: u32,
        end: u32,
    ) -> Result<(), RequestError<SpCtrlError>> {
        if !self.is_swd_setup() {
            return Err(SpCtrlError::NeedInit.into());
        }
        ringbuf_entry!(Trace::ReadX {
            start,
            len: end - start
        });
        self.start_read_transaction(start, ((end - start) as usize) / 4)
            .map_err(|e| e.into())
    }

    fn read_transaction(
        &mut self,
        _: &RecvMessage,
        dest: LenLimit<Leased<W, [u8]>, 4096>,
    ) -> Result<(), RequestError<SpCtrlError>> {
        if !self.is_swd_setup() {
            return Err(SpCtrlError::NeedInit.into());
        }

        let cnt = dest.len();
        if !cnt.is_multiple_of(4) {
            return Err(SpCtrlError::BadLen.into());
        }
        let mut buf = LeaseBufWriter::<_, 32>::from(dest.into_inner());

        for _ in 0..cnt / 4 {
            if let Some(w) = self.read_transaction_word()? {
                ringbuf_entry!(Trace::MemVal(w));
                for b in w.to_le_bytes() {
                    if buf.write(b).is_err() {
                        return Ok(());
                    }
                }
            }
        }

        Ok(())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, 4096>,
    ) -> Result<(), RequestError<SpCtrlError>> {
        ringbuf_entry!(Trace::ReadCmd);
        if !self.is_swd_setup() {
            return Err(SpCtrlError::NeedInit.into());
        }
        let cnt = dest.len();
        if !cnt.is_multiple_of(4) {
            return Err(SpCtrlError::BadLen.into());
        }
        let mut buf = LeaseBufWriter::<_, 32>::from(dest.into_inner());

        for i in 0..cnt / 4 {
            let r = self.read_single_target_addr(addr + ((i * 4) as u32))?;
            ringbuf_entry!(Trace::MemVal(r));
            for b in r.to_le_bytes() {
                if buf.write(b).is_err() {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    fn write(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<R, [u8]>, 4096>,
    ) -> Result<(), RequestError<SpCtrlError>> {
        ringbuf_entry!(Trace::WriteCmd);
        if !self.is_swd_setup() {
            return Err(SpCtrlError::NeedInit.into());
        }
        let cnt = dest.len();
        if !cnt.is_multiple_of(4) {
            return Err(SpCtrlError::BadLen.into());
        }
        let mut buf = LeaseBufReader::<_, 32>::from(dest.into_inner());

        for i in 0..cnt / 4 {
            let mut word: [u8; 4] = [0; 4];
            for item in &mut word {
                match buf.read() {
                    Some(b) => *item = b,
                    None => return Ok(()),
                };
            }
            self.write_single_target_addr(
                addr + ((i * 4) as u32),
                u32::from_le_bytes(word),
            )?;
        }

        Ok(())
    }

    fn setup(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        self.do_setup_swd()?;
        Ok(())
    }

    fn halt(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        self.do_halt()?;
        Ok(())
    }

    fn resume(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        self.do_resume()?;
        Ok(())
    }

    fn read_core_register(
        &mut self,
        _: &RecvMessage,
        register: u16,
    ) -> Result<u32, RequestError<SpCtrlError>> {
        let r =
            Reg::from_u16(register).ok_or(SpCtrlError::InvalidCoreRegister)?;
        self.write_single_target_addr(DCRSR, r as u32)
            .map_err(SpCtrlError::from)?;
        loop {
            let dhcsr = self.dp_read_bitflags::<Dhcsr>()?;
            ringbuf_entry!(Trace::Dhcsr(dhcsr));

            if dhcsr.is_regrdy() {
                break;
            }
        }

        self.read_dcrdr().map_err(|e| e.into())
    }

    fn enable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
        time_ms: u32,
    ) -> Result<(), RequestError<SpCtrlError>> {
        ringbuf_entry!(Trace::EnabledWatchdog);
        if !self.is_swd_setup() {
            // The init will fail if there is an active debug dongle on the SP
            // SWD interface.
            // If there was an active dongle, then the SWD interface would not
            // be usable to the RoT when when needed.
            // This is a clue to the SP's update_server client that they should
            // not proceed.
            return Err(SpCtrlError::NeedInit.into());
        }
        // This function is idempotent(ish), so we don't care if the timer was
        // already running; set the new deadline based on current time.
        set_timer_relative(time_ms, notifications::TIMER_MASK);

        // The common case is that there will be a RESET before the watchdog
        // can fire.
        // That will kick off an SP image measurement which takes under a second.
        //
        // At the time of writing this comment, the watchdog timer value used
        // in omicron is 2000ms. With different startup times for the various
        // SP Hubris applications, it is important to test for watchdog failure
        // cases.
        Ok(())
    }

    fn disable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Trace::DisabledWatchdog);
        sys_set_timer(None, notifications::TIMER_MASK);
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
            + notifications::SP_RESET_IRQ_MASK
            + notifications::JTAG_DETECT_IRQ_MASK
    }

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        // If JTAG_DETECT fires:
        //   - invalidate any SP measurement
        //   - if still asserted, then the other handlers will fail on
        //     their calls to do_setup_swd();
        //   - We could try extending the Watchdog timer if it is active in hopes
        //     that the SP dongle is removed or its power is removed, but if
        //     there is a dongle attached to the SP, then let the humans figure it out
        //     and don't complicate the behavior here.

        if bits.check_notification_mask(notifications::JTAG_DETECT_IRQ_MASK) {
            ringbuf_entry!(Trace::SpJtagDetectFired);
            const SLOT: PintSlot = SP_TO_ROT_JTAG_DETECT_L_PINT_SLOT;
            if self.gpio.pint_detect(SLOT, PintFlag::Falling) {
                ringbuf_entry!(Trace::InvalidateSpMeasurement);
                self.on_swd_connected();
                self.gpio.pint_clear(SLOT, PintFlag::Both);
            } else {
                // Spurious notification, ignore it
            }
            sys_irq_control(notifications::JTAG_DETECT_IRQ_MASK, true);
        } else if matches!(self.state, SwdState::Disconnected)
            && !self.swd_dongle_detected()
        {
            self.on_swd_disconnected();
        }

        if bits.has_timer_fired(notifications::TIMER_MASK) {
            ringbuf_entry!(Trace::WatchdogFired);

            match self.do_setup_swd() {
                Ok(()) => {
                    // Disable the watchdog timer
                    sys_set_timer(None, notifications::TIMER_MASK);
                    if let Err(e) = self.do_setup_swd() {
                        // This is potentially bad if we really need
                        // to activate the alternate SP flash bank and
                        // are not able to do that.
                        //
                        // It means that that there is:
                        //   - an SWD logic bug, or
                        //   - something is wrong with the SWD signals, or
                        //   - in the time between the WD timer being set and
                        //     the timer firing, a JTAG dongle has been
                        //     powered-up or attached.
                        //
                        // These problems either should have been caught in CI
                        // testing, require HW repair, or are because a human is
                        // messing with the system by holding down the SP's
                        // physical reset button.
                        //
                        // Reporting:
                        // RFDs 544 and 520 may result in a reporting
                        // mechanism that can be used here.
                        // RFD 440 discusses improving robustness.
                        // Until then, just log in the ringbuf.
                        ringbuf_entry!(Trace::TimerHandlerError(e));
                    } else {
                        // Attempt to do the swap
                        let r = self.swap_sp_slot();
                        // r.is_err() is potential bad. See comment above.
                        ringbuf_entry!(Trace::WatchdogSwap(r));
                        // Force next user to re-initialize the SWD interface.
                        self.next_use_must_setup_swd();
                    }
                }
                Err(e) => {
                    // This should only fail if JTAG_DETECT or SP_RESET are
                    // currently asserted.
                    ringbuf_entry!(Trace::TimerHandlerError(e));
                }
            }
        }

        if bits.check_notification_mask(notifications::SP_RESET_IRQ_MASK) {
            //  We are not going to try to measure/trust the SP when there is a
            //  glitch on the JTAG_DETECT signal.
            //
            //  e.g. JTAG_DETECT fired but before the handler was called, it
            //  de-asserted so that the SP_RESET that also fired could be
            //  handled successfully.
            ringbuf_entry!(Trace::SpResetFired);
            if matches!(self.state, SwdState::Connected { .. })
                && !self.do_handle_sp_reset()
            {
                // If handling the SP reset failed, clear the attestation log
                ringbuf_entry!(Trace::InvalidateSpMeasurement);
                self.invalidate_sp_measurement();
            }
            self.next_use_must_setup_swd();

            // Squelch the interrupts generated by this handler's toggling of
            // SP_RESET.
            self.gpio
                .pint_clear(ROT_TO_SP_RESET_L_IN_PINT_SLOT, PintFlag::Falling);
            sys_irq_control_clear_pending(
                notifications::SP_RESET_IRQ_MASK,
                true,
            );

            ringbuf_entry!(Trace::EndOfNotificationHandler);
        }
    }
}

impl ServerImpl {
    /// Callback to handle when an external programmer is connected
    ///
    /// This function invalidates the measurement log, switches our internal
    /// state, and configures the SPI pins as inputs (to avoid fighting with the
    /// programmer).
    fn on_swd_connected(&mut self) {
        // Reset the attestation log
        self.invalidate_sp_measurement();

        // Cancel ongoing transactions
        self.transaction = None;
        self.state = SwdState::Disconnected;
        switch_io_disconnected();
    }

    /// When an external programmer is disconnected, connect to the SP
    fn on_swd_disconnected(&mut self) {
        switch_io_connected();
        self.state = SwdState::Connected { init: false };
    }

    fn io_out(&mut self) {
        self.wait_for_mstidle();
        switch_io_out();
    }

    fn io_in(&mut self) {
        self.wait_for_mstidle();
        switch_io_in();
    }

    fn read_ack(&mut self) -> Result<(), Ack> {
        // This read includes the turnaround bit which we
        // don't care about.
        let b = self.read_nibble();

        // We configured the SPI controller to give us back 4 bits,
        // if we got more than that something has gone very wrong
        if b & 0xF0 != 0 {
            ringbuf_entry!(Trace::AckErr(Ack::Protocol));
            return Err(Ack::Protocol);
        }

        // Section 5.3 of ADIv5 describes the bit patterns
        match b & 0x7 {
            0b001 => {
                ringbuf_entry!(Trace::AckErr(Ack::Fault));
                Err(Ack::Fault)
            }
            0b010 => {
                ringbuf_entry!(Trace::AckErr(Ack::Wait));
                Err(Ack::Wait)
            }
            0b100 => Ok(()),
            _ => {
                ringbuf_entry!(Trace::AckErr(Ack::Protocol));
                Err(Ack::Protocol)
            }
        }
    }

    // We purposely poll on these functions instead of waiting for an interrupt
    // because the overhead of the system calls is much higher than the number
    // of cycles we expect to wait given the throughput.

    fn wait_to_tx(&mut self) {
        while !self.spi.can_tx() {
            cortex_m::asm::nop();
        }
    }

    fn wait_for_rx(&mut self) {
        while !self.spi.has_entry() {
            cortex_m::asm::nop();
        }
    }

    fn wait_for_mstidle(&mut self) {
        while !self.spi.mstidle() {
            cortex_m::asm::nop();
        }
    }

    fn tx_byte(&mut self, byte: u8) {
        self.wait_to_tx();
        self.spi.send_u8_no_rx(byte);
    }

    // SW-DP is intended to be used as a bit based protocol.
    // The smallest unit the SPI controller can do is 4 bits
    fn read_nibble(&mut self) -> u8 {
        self.wait_to_tx();
        self.spi.send_raw_data(0x0, true, false, 4);
        self.wait_for_rx();
        self.spi.read_u8()
    }

    fn read_byte(&mut self) -> u8 {
        self.wait_to_tx();
        self.spi.send_raw_data(0x0, true, false, 8);
        self.wait_for_rx();
        self.spi.read_u8()
    }

    fn read_nine_bits(&mut self) -> u16 {
        self.wait_to_tx();
        self.spi.send_raw_data(0x0, true, false, 9);
        self.wait_for_rx();
        self.spi.read_u16()
    }

    fn swd_transfer_cmd(
        &mut self,
        port: Port,
        reg: RawSwdReg,
    ) -> Result<(), Ack> {
        self.io_out();

        // has our start and stop bits set
        let mut byte: u8 = START_VAL | PARK_VAL;

        let (rd, abits) = get_addr_and_rw(reg);

        let port_bit: u8 = match port {
            Port::DP => 0 << APDP_BIT,
            Port::AP => 1 << APDP_BIT,
        };

        byte |= abits | rd | port_bit;

        let p = calc_parity(byte);

        byte |= p;

        self.tx_byte(byte);

        self.io_in();

        self.read_ack()
    }

    fn reset(&mut self) {
        // Spec says hold high for 50 clock cycles, more is okay
        // this gives us 56
        for _ in 0..7 {
            self.tx_byte(0xff);
        }
    }

    fn idle_cycles(&mut self, cnt: usize) {
        // Transmitting one bit = one idle cycle, convert bytes to bits
        // for the correct count.
        //
        // Round up here just to be safe
        let rounded = cnt.div_ceil(8) * 8;
        for _ in 0..(rounded / 8) {
            self.tx_byte(0x00);
        }
    }

    #[inline(never)]
    fn swd_switch(&mut self) {
        // Section B5.2.2 of ADIv6 specifies this constant
        // This is the MSB version. If this ever switches to LSB transmission
        // this should be updated!
        const JTAG_MAGIC: u16 = 0x79E7;

        self.wait_to_tx();
        self.spi.send_raw_data(JTAG_MAGIC, true, true, 16);
    }

    fn read_word(&mut self) -> Option<u32> {
        let mut result: u32 = 0;

        self.io_in();

        let mut parity = 0;

        // We need to read exactly 33 bits. We have MOSI disabled so trying to
        // read more results in protocol errors because we can't appropriately
        // drive the line low to treat it as extra idle cycles.
        for i in 0..4 {
            let b = if i == 3 {
                // The last read is 9 bits. Right now we just shift the parity
                // bit away because it's not clear what the appropriate
                // response is if we detect a parity error. "Might have to
                // re-issue original read request or use the RESEND register if
                // a parity or protocol fault" doesn't give much of a hint...
                let val = self.read_nine_bits();
                parity = val & 1;
                ((val >> 1).reverse_bits() >> 8) as u32
            } else {
                (self.read_byte().reverse_bits()) as u32
            };
            result |= b << (i * 8);
        }

        if result.count_ones() % 2 != (parity as u32) {
            ringbuf_entry!(Trace::ParityFail {
                data: result,
                received_parity: parity
            });
            None
        } else {
            Some(result)
        }
    }

    fn write_word(&mut self, val: u32) {
        let parity: u32 = u32::from(!val.count_ones().is_multiple_of(2));

        let rev = val.reverse_bits();

        let first: u16 = (rev >> 24 & 0xFF) as u16;
        let second: u16 = (rev >> 16 & 0xFF) as u16;
        let third: u16 = (rev >> 8 & 0xFF) as u16;
        let fourth: u16 = (((rev & 0xFF) << 1) | parity) as u16;

        // We're going to transmit 34 bits: one bit of turnaround (i.e.
        // don't care), 32 bits of data and one bit of parity.
        // Break this up by transmitting 9 bits (turnaround + first byte)
        // 8 bits, 8 bits, 9 bits (last byte + parity)

        self.spi.send_raw_data(first, true, true, 9);
        self.spi.send_raw_data(second, true, true, 8);
        self.spi.send_raw_data(third, true, true, 8);
        self.spi.send_raw_data(fourth, true, true, 9);
    }

    fn swd_read(&mut self, port: Port, reg: RawSwdReg) -> Result<u32, Ack> {
        loop {
            let result = self.swd_transfer_cmd(port, reg);

            match result {
                Ok(_) => (),
                Err(e) => {
                    // Need to handle the turnaround bit
                    self.io_out();
                    self.idle_cycles(8);
                    match e {
                        Ack::Wait => continue,
                        _ => return Err(e),
                    }
                }
            }

            let ret = self.read_word();

            self.io_out();

            // These cycles are absolutely necessary on a read to account
            // for the required turnaround bit!
            self.swd_finish();

            return ret.ok_or(Ack::Fault);
        }
    }

    fn swd_dongle_detected(&self) -> bool {
        self.gpio.read_val(SP_TO_ROT_JTAG_DETECT_L) == Value::Zero
    }

    fn swd_setup(&mut self) -> Result<(), SwdSetupErr> {
        self.io_out();
        // Section B5.2.2 of ADIv6 specifies this sequence
        self.reset();
        self.swd_switch();
        self.reset();

        self.idle_cycles(16);

        // Must read DP IDCODE register after reset
        let result = self
            .swd_read(Port::DP, RawSwdReg::DpRead(DpRead::IDCode))
            .map_err(SwdSetupErr::IdCode)?;

        ringbuf_entry!(Trace::Idcode(result));

        self.power_up().map_err(SwdSetupErr::PowerUp)?;

        // Read the IDR as a basic test for reading from the AP
        let result = self
            .swd_read_ap_reg(ApAddr(0, ApReg::IDR), false)
            .map_err(SwdSetupErr::Idr)?;
        ringbuf_entry!(Trace::Idr(result));

        Ok(())
    }

    fn swd_finish(&mut self) {
        // Allow some idle cycles
        self.idle_cycles(8);
    }

    fn swd_write(
        &mut self,
        port: Port,
        reg: RawSwdReg,
        val: u32,
    ) -> Result<(), Ack> {
        loop {
            let result = self.swd_transfer_cmd(port, reg);

            if let Err(e) = result {
                // Need to account for the turnaround bit before continuing
                self.io_out();
                self.idle_cycles(8);
                match e {
                    Ack::Wait => continue,
                    _ => return Err(e),
                }
            }

            self.io_out();
            self.write_word(val);
            self.swd_finish();
            return Ok(());
        }
    }

    fn swd_write_ap_reg(
        &mut self,
        addr: ApAddr,
        val: u32,
        skip_sel: bool,
    ) -> Result<(), Ack> {
        let ap_sel = addr.0 << 24;
        let bank_sel = (addr.1 as u32) & 0xF0;

        if !skip_sel {
            self.swd_write(
                Port::DP,
                RawSwdReg::DpWrite(DpWrite::Select),
                ap_sel | bank_sel,
            )?;
        }

        self.swd_write(Port::AP, RawSwdReg::ApWrite(addr.1), val)
    }
    fn swd_read_ap_reg(
        &mut self,
        addr: ApAddr,
        skip_sel: bool,
    ) -> Result<u32, Ack> {
        let ap_sel = addr.0 << 24;
        let bank_sel = (addr.1 as u32) & 0xF0;

        if !skip_sel {
            self.swd_write(
                Port::DP,
                RawSwdReg::DpWrite(DpWrite::Select),
                ap_sel | bank_sel,
            )?;
        }

        // See section 6.2.5 ADIV5
        // If you require the value from an AP register read, that read must be
        // followed by one of:
        // - A second AP register read, with the appropriate AP selected as the
        //   current AP.
        // - A read of the DP Read Buffer
        //
        // We intentionally take the DP read buffer option to avoid screwing up
        // the auto incrementing TAR register
        let _ = self.swd_read(Port::AP, RawSwdReg::ApRead(addr.1))?;

        let val = self.swd_read(Port::DP, RawSwdReg::DpRead(DpRead::Rdbuf))?;

        Ok(val)
    }

    fn start_read_transaction(
        &mut self,
        addr: u32,
        word_cnt: usize,
    ) -> Result<(), Ack> {
        // The transaction size limit is 1k, see C2.2.2 of ADIv5
        const TRANSACTION_LIMIT: usize = 1024;
        // Check against the number of 32-bit words we expect to read
        if word_cnt > TRANSACTION_LIMIT / 4 {
            return Err(Ack::Fault);
        }
        self.clear_errors()?;

        self.swd_write_ap_reg(
            ApAddr(0, ApReg::CSW),
            CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC | CSW_SIZE32,
            false,
        )?;

        self.swd_write_ap_reg(ApAddr(0, ApReg::TAR), addr, false)?;

        self.transaction = Some(MemTransaction {
            total_word_cnt: word_cnt,
            read_cnt: 0,
        });

        Ok(())
    }

    /// Write an arbitrary number of words via SWD
    fn swd_bulk_write(&mut self, addr: u32, data: &[u8]) -> Result<(), Ack> {
        // TODO: Performance could be improved here. This is using
        // single_read/single_write to write and verify every u32.
        // It is consuming about 0.25 seconds to inject the `endoscope` code.
        let mut addr = addr;
        const U32_SIZE: usize = core::mem::size_of::<u32>();
        if !data.len().is_multiple_of(U32_SIZE) {
            ringbuf_entry!(Trace::BadLen);
            return Err(Ack::Fault);
        }
        for slice in data.chunks_exact(U32_SIZE) {
            if let Some(word) = slice_to_le_u32(slice) {
                let mut limit = 2;
                loop {
                    self.write_single_target_addr(addr, word)?;
                    let readback = self.read_single_target_addr(addr)?;
                    if readback == word {
                        break;
                    }
                    ringbuf_entry!(Trace::Data {
                        addr,
                        data: readback,
                        src: word
                    });
                    if limit == 0 {
                        ringbuf_entry!(Trace::ReadbackFailure);
                        return Err(Ack::Fault);
                    }
                    limit -= 1;
                }
                addr += U32_SIZE as u32;
            } else {
                // Using chunks_exact means that the conversion to u32 will succeed.
                unreachable!();
            }
        }
        Ok(())
    }

    fn read_transaction_word(&mut self) -> Result<Option<u32>, Ack> {
        if let Some(mut transaction) = &self.transaction {
            let val = self.swd_read_ap_reg(ApAddr(0, ApReg::DRW), true)?;

            transaction.read_cnt += 1;
            if transaction.read_cnt == transaction.total_word_cnt {
                self.transaction = None;
            } else {
                self.transaction = Some(transaction);
            }
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    fn read_single_target_addr(&mut self, addr: u32) -> Result<u32, Ack> {
        if self.transaction.is_some() {
            return Err(Ack::Fault);
        }

        self.clear_errors()?;
        self.swd_write_ap_reg(
            ApAddr(0, ApReg::CSW),
            CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC | CSW_SIZE32,
            false,
        )?;

        self.swd_write_ap_reg(ApAddr(0, ApReg::TAR), addr, false)?;

        let val = self.swd_read_ap_reg(ApAddr(0, ApReg::DRW), false)?;

        Ok(val)
    }

    /// Read the DP's 32-bit data register
    fn read_dcrdr(&mut self) -> Result<u32, Ack> {
        self.read_single_target_addr(DCRDR)
    }

    /// Read a DP register as a known bitflag.
    fn dp_read_bitflags<T>(&mut self) -> Result<T, Ack>
    where
        T: bitflags::Flags<Bits = u32> + DpAddressable,
    {
        self.read_single_target_addr(T::ADDRESS)
            .map(|r| T::from_bits_retain(r))
    }

    fn dp_write_bitflags<T>(&mut self, val: T) -> Result<(), Ack>
    where
        T: bitflags::Flags<Bits = u32> + DpAddressable,
    {
        self.write_single_target_addr(T::ADDRESS, val.bits())
    }

    fn write_single_target_addr(
        &mut self,
        addr: u32,
        val: u32,
    ) -> Result<(), Ack> {
        if self.transaction.is_some() {
            return Err(Ack::Fault);
        }

        self.clear_errors()?;

        self.swd_write_ap_reg(
            ApAddr(0, ApReg::CSW),
            CSW_HPROT | CSW_DBGSTAT | CSW_SADDRINC | CSW_SIZE32,
            false,
        )?;

        self.swd_write_ap_reg(ApAddr(0, ApReg::TAR), addr, false)?;

        self.swd_write_ap_reg(ApAddr(0, ApReg::DRW), val, false)?;

        Ok(())
    }

    fn clear_errors(&mut self) -> Result<(), Ack> {
        self.swd_write(Port::DP, RawSwdReg::DpWrite(DpWrite::Abort), 0x1F)
    }

    fn power_up(&mut self) -> Result<(), Ack> {
        self.clear_errors()?;
        self.swd_write(Port::DP, RawSwdReg::DpWrite(DpWrite::Select), 0x0)?;
        self.swd_write(
            Port::DP,
            RawSwdReg::DpWrite(DpWrite::Ctrl),
            DP_CTRL_CDBGPWRUPREQ,
        )?;

        loop {
            let r = self.swd_read(Port::DP, RawSwdReg::DpRead(DpRead::Ctrl))?;
            if r & DP_CTRL_CDBGPWRUPACK == DP_CTRL_CDBGPWRUPACK {
                break;
            }
        }

        Ok(())
    }

    /// Swaps the currently-active SP slot
    fn swap_sp_slot(&mut self) -> Result<(), Ack> {
        // All registers and constants are within the FLASH peripheral block, so
        // I'm going to skip prefixing everything with `FLASH_`.

        // RM0433 Table 8
        const BASE: u32 = 0x52002000;

        // RM0433 Section 4.9.7
        const OPTCR: u32 = BASE + 0x018;
        const OPTCR_OPTLOCK_BIT: u32 = 1 << 0;
        const OPTCR_OPTSTART_BIT: u32 = 1 << 1;

        // Check whether we have to unlock the flash control register
        let optcr = self.read_single_target_addr(OPTCR)?;
        if optcr & OPTCR_OPTLOCK_BIT != 0 {
            // Keys constants are defined in RM0433 Rev 7
            // Section 4.9.3
            const OPT_KEY1: u32 = 0x0819_2A3B;
            const OPT_KEY2: u32 = 0x4C5D_6E7F;
            const OPTKEYR: u32 = BASE + 0x008;
            self.write_single_target_addr(OPTKEYR, OPT_KEY1)?;
            self.write_single_target_addr(OPTKEYR, OPT_KEY2)?;
        }

        // Read the current bank swap bit
        const OPTSR_CUR: u32 = BASE + 0x01C;
        const OPTSR_SWAP_BANK_OPT_BIT: u32 = 1 << 31;
        const OPTSR_OPT_BUSY_BIT: u32 = 1 << 0;
        let optsr_cur = self.read_single_target_addr(OPTSR_CUR)?;

        // Mask and toggle the bank swap bit
        let new_swap =
            (optsr_cur & OPTSR_SWAP_BANK_OPT_BIT) ^ OPTSR_SWAP_BANK_OPT_BIT;

        // Modify the bank swap bit in OPTSR_PRG
        const OPTSR_PRG: u32 = BASE + 0x020;
        let mut optsr_prg = self.read_single_target_addr(OPTSR_PRG)?;
        optsr_prg = (optsr_prg & !OPTSR_SWAP_BANK_OPT_BIT) | new_swap;
        self.write_single_target_addr(OPTSR_PRG, optsr_prg)?;

        // Start programming option bits
        let mut optcr = self.read_single_target_addr(OPTCR)?;
        optcr |= OPTCR_OPTSTART_BIT;
        self.write_single_target_addr(OPTCR, optcr)?;

        // Wait for option bit programming to finish
        while self.read_single_target_addr(OPTSR_CUR)? & OPTSR_OPT_BUSY_BIT != 0
        {
            hl::sleep_for(5);
        }

        // Reset the STM32, causing it to reboot into the newly-set slot
        self.sp_reset();
        Ok(())
    }

    fn sp_reset(&mut self) {
        self.sp_reset_enter();
        hl::sleep_for(10);
        self.sp_reset_leave();
    }

    fn sp_reset_enter(&mut self) {
        setup_rot_to_sp_reset_l_out(self.gpio_task);
        self.gpio.set_val(ROT_TO_SP_RESET_L_OUT, Value::Zero);
    }

    fn sp_reset_leave(&mut self) {
        setup_rot_to_sp_reset_l_in(self.gpio_task);
        self.gpio.set_val(ROT_TO_SP_RESET_L_IN, Value::One); // should be a no-op
    }

    // The SP is halted at its reset vector.
    fn sp_measure_fast(&mut self) -> Result<[u8; 256 / 8], ()> {
        // write program entry address to PC (R15)
        // write top-of-stack address to MSP (R13)
        // write start of image/vector table address(0x20000000) to VTOR (at 0xe000ed08)
        // write the program to RAM
        // write DHCSR to RUN (MATIC + C_DEBUGEN)
        // poll for S_HALT or timeout

        // Search st.com for document "PM0253". Section 2.4.4 describes
        // the STM32H753 vector table.
        //
        // The `endoscope` image has the vector table at offset 0. When loaded
        // into the SP, that table will be at `endoscope::LOAD` and the SP's
        // `VTOR` register will be set to that address prior to the SP being
        // released from reset. Addresses in the vector table are absolute
        // runtime values.
        //
        // The first u32 in the `endoscope` image is the initial stack pointer
        // value. The second u32 is the initial program counter, a.k.a. the
        // reset vector.

        const_assert!(ENDOSCOPE_BYTES.len() > 2 * 1024);

        // Set SP's Program Counter
        let sp_reset_vector =
            slice_to_le_u32(&ENDOSCOPE_BYTES[4..=7]).unwrap_lite();
        if self
            .do_write_core_register(Reg::Dr, sp_reset_vector)
            .is_err()
        {
            ringbuf_entry!(Trace::WrotePcRegisterFail);
            return Err(());
        }

        // Set SP's Stack Pointer
        let sp_initial_sp =
            slice_to_le_u32(&ENDOSCOPE_BYTES[0..=3]).unwrap_lite();
        if self.do_write_core_register(Reg::Sp, sp_initial_sp).is_err() {
            ringbuf_entry!(Trace::WroteSpRegisterFail);
            return Err(());
        }

        // Set VTOR - Set vector table base address
        if self
            .write_single_target_addr(VTOR, endoscope::LOAD)
            .is_err()
        {
            ringbuf_entry!(Trace::CannotWriteVtor);
            return Err(());
        }

        // Write the endoscope program into the SP RAM
        let start = sys_get_timer().now;
        if self
            .swd_bulk_write(endoscope::LOAD, ENDOSCOPE_BYTES)
            .is_err()
        {
            ringbuf_entry!(Trace::InjectionFailed);
            return Err(());
        }
        // log the injection time which can still be improved.
        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::Injected {
            start: endoscope::LOAD,
            length: ENDOSCOPE_BYTES.len(),
            delta_t: (now.saturating_sub(start)) as u32
        });

        // Resume execution by turning off DHCSR_C_HALT
        if let Err(e) = self.dp_write_bitflags::<Dhcsr>(Dhcsr::resume()) {
            ringbuf_entry!(Trace::AckErr(e));
            return Err(());
        }

        // It takes about 0.25 seconds (236 RoT systicks) for `endoscope` to run.
        // Allow about twice that time for the measurement to complete.
        // endoscope executes a BKPT instruction on completion.
        // We observe an S_HALT state if all goes well.
        // Otherwise, time out due to not halting as expected,
        // or faulting before setting the proper shared state
        // result in returning failures.

        // Note: If you are doing manual testing and initiating a measurement by pushing an "SP
        // RESET" button, you can induce a failure to measure by holding down the reset button for
        // longer than this timeout. So, unless that is what you want, release the button before
        // 0.5 seconds is up.
        if self.wait_for_sp_halt(WAIT_FOR_HALT_MS).is_err() {
            // If a human is holding down a physical reset button then
            // SP_RESET may have never been released.
            let sp_reset_state = self.gpio.read_val(ROT_TO_SP_RESET_L_IN);
            ringbuf_entry!(Trace::DidNotHalt { sp_reset_state });
            return Err(());
        };

        let mut shared = Shared {
            state: State::Preboot as u32,
            digest: [0u8; 32],
        };

        if self
            .read_buf_from_addr(endoscope::SHARED, shared.as_mut_bytes())
            .is_err()
        {
            ringbuf_entry!(Trace::ReadBufFail);
            return Err(());
        }

        ringbuf_entry!(Trace::SharedState(shared.state));
        if shared.state != (State::Done as u32) {
            return Err(());
        }

        for (i, chunk) in shared.digest.chunks_exact(8).enumerate() {
            let d = chunk.try_into().unwrap_lite();
            ringbuf_entry!(Trace::Digest(i, d));
        }

        Ok(shared.digest)
    }

    // C1.6 Debug system registers
    fn do_write_core_register(
        &mut self,
        register: Reg,
        value: u32,
    ) -> Result<(), SpCtrlError> {
        self.write_single_target_addr(DCRDR, value)
            .map_err(SpCtrlError::from)?;
        self.write_single_target_addr(DCRSR, register as u32 | (1u32 << 16))
            .map_err(SpCtrlError::from)?;

        const RETRY_LIMIT: u32 = 10;
        let mut limit = RETRY_LIMIT;
        loop {
            let dhcsr = self.dp_read_bitflags::<Dhcsr>()?;
            if dhcsr.is_regrdy() {
                // Trace retries used
                if limit != RETRY_LIMIT {
                    ringbuf_entry!(Trace::LimitRemaining(limit));
                }
                return Ok(());
            }
            if limit == 0 {
                ringbuf_entry!(Trace::LimitRemaining(limit));
                return Err(SpCtrlError::Fault);
            }
            limit -= 1;
            hl::sleep_for(1);
        }
    }

    /// Measure the current SP Hubris Image.
    /// The SP reset vector has been trapped and
    /// the SP is halted.
    fn do_measure_sp(&mut self) -> Result<[u8; 256 / 8], ()> {
        // For Hubris on the STM32H7, The FWID includes 0xff padding to
        // the end of the flash bank.

        // Time the code injection injection, calculation,
        // and readout of the FWID.
        let start = sys_get_timer().now;
        let measurement_result = self.sp_measure_fast();

        ringbuf_entry!(Trace::MeasuredSp {
            success: measurement_result.is_ok(),
            delta_t: (sys_get_timer().now.saturating_sub(start)) as u32
        });

        measurement_result
    }

    fn read_buf_from_addr(
        &mut self,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), SpCtrlError> {
        let start = addr;
        let end = addr + buf.len() as u32;
        self.start_read_transaction(start, ((end - start) as usize) / 4)
            .map_err(SpCtrlError::from)?;

        let cnt = buf.len();
        if !cnt.is_multiple_of(4) {
            return Err(SpCtrlError::BadLen);
        }

        let mut i = 0usize;
        for _ in 0..cnt / 4 {
            if let Some(w) = self.read_transaction_word()? {
                for b in w.to_le_bytes() {
                    buf[i] = b;
                    i += 1;
                }
            }
        }

        Ok(())
    }

    fn do_setup_swd(&mut self) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::DoSetup);

        if self.swd_dongle_detected() {
            // Don't change state here; we'll catch it in the pin-change IRQ
            ringbuf_entry!(Trace::DongleDetected);
            return Err(SpCtrlError::DongleDetected);
        } else if self.state == SwdState::Disconnected {
            switch_io_connected();
            self.state = SwdState::Connected { init: false };
        }

        match self.swd_setup() {
            Ok(_) => {
                ringbuf_entry!(Trace::SwdSetupOk);
                self.state = SwdState::Connected { init: true };
                Ok(())
            }
            Err(e) => {
                // Other parts of the ringbuf are more useful, no need to repeat
                ringbuf_entry!(Trace::SwdSetupFail);
                match e {
                    SwdSetupErr::IdCode(Ack::Fault) => {
                        Err(SpCtrlError::IdCodeFault)
                    }
                    SwdSetupErr::IdCode(Ack::Wait) => {
                        Err(SpCtrlError::IdCodeWait)
                    }
                    SwdSetupErr::IdCode(Ack::Protocol) => {
                        Err(SpCtrlError::IdCodeProtocol)
                    }
                    SwdSetupErr::PowerUp(Ack::Fault) => {
                        Err(SpCtrlError::PowerUpFault)
                    }
                    SwdSetupErr::PowerUp(Ack::Wait) => {
                        Err(SpCtrlError::PowerUpWait)
                    }
                    SwdSetupErr::PowerUp(Ack::Protocol) => {
                        Err(SpCtrlError::PowerUpProtocol)
                    }
                    SwdSetupErr::Idr(Ack::Fault) => Err(SpCtrlError::IdrFault),
                    SwdSetupErr::Idr(Ack::Wait) => Err(SpCtrlError::IdrWait),
                    SwdSetupErr::Idr(Ack::Protocol) => {
                        Err(SpCtrlError::IdrProtocol)
                    }
                }
            }
        }
    }

    fn next_use_must_setup_swd(&mut self) {
        // Reset SWD state if it is connected
        if let SwdState::Connected { init } = &mut self.state {
            *init = false;
        }
        // Any in-progress bulk data transfer is cancelled.
        self.transaction = None;
    }

    fn is_swd_setup(&self) -> bool {
        matches!(self.state, SwdState::Connected { init: true })
    }

    fn do_halt(&mut self) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::DoHalt);
        self.dp_write_bitflags::<Dhcsr>(Dhcsr::halt())
            .map_err(SpCtrlError::from)?;
        self.wait_for_sp_halt(WAIT_FOR_HALT_MS)
    }

    fn wait_for_sp_halt(&mut self, timeout: u64) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::WaitingForSpHalt {
            timeout: timeout as u32
        });
        let start = sys_get_timer().now;
        let deadline = start.wrapping_add(timeout);
        loop {
            match self.dp_read_bitflags::<Dhcsr>() {
                Ok(dhcsr) => {
                    if dhcsr.is_halted() {
                        ringbuf_entry!(Trace::Halted {
                            delta_t: (sys_get_timer().now.saturating_sub(start))
                                as u32
                        });
                        return Ok(());
                    }
                }
                Err(e) => {
                    ringbuf_entry!(Trace::HaltFail(
                        (sys_get_timer().now.saturating_sub(start)) as u32
                    ));
                    return Err(e.into());
                }
            };
            if deadline <= sys_get_timer().now {
                // If a human is holding down a physical reset button then
                // SP_RESET may have never been released.
                let sp_reset_state = self.gpio.read_val(ROT_TO_SP_RESET_L_IN);
                ringbuf_entry!(Trace::DidNotHalt { sp_reset_state });
                break Err(SpCtrlError::Timeout);
            }
            hl::sleep_for(1);
        }
    }

    fn do_resume(&mut self) -> Result<(), SpCtrlError> {
        if let Err(e) = self.dp_write_bitflags::<Dhcsr>(Dhcsr::resume()) {
            ringbuf_entry!(Trace::ResumeFail);
            Err(e.into())
        } else {
            ringbuf_entry!(Trace::Resumed);
            Ok(())
        }
    }

    /// Invalidates the SP measurement, logging the result
    fn invalidate_sp_measurement(&mut self) {
        match self.attest.reset() {
            Ok(()) => ringbuf_entry!(Trace::InvalidatedSpMeasurement),
            Err(e) => ringbuf_entry!(Trace::AttestError(e)),
        }
    }

    // Return true if necessary work was done.
    // Return false if any current SP measurement should be invalidated.
    #[must_use]
    fn do_handle_sp_reset(&mut self) -> bool {
        // Did SP_RESET transition to Zero?
        if !self
            .gpio
            .pint_detect(ROT_TO_SP_RESET_L_IN_PINT_SLOT, PintFlag::Falling)
        {
            // Use of sys_irq_control_clear_pending(...) should avoid appearance
            // of a "spurious" interrupt.
            //
            // Otherwise, cases where we assert SP_RESET then clean-up the PINT
            // condition will have a pending notification.
            ringbuf_entry!(Trace::SpResetNotAsserted);
            return true; // no work required.
        }

        ringbuf_entry!(Trace::SpResetAsserted);

        // This notification handler should be compatible with watchdog but will
        // result in the invalidation of any dumps held in the SP if successful.

        // TODO: confirm that bank-flipping SP update watchdog is working.

        if self.do_setup_swd().is_ok() {
            ringbuf_entry!(Trace::SetupSwdOk);
        } else {
            // We may have interrupted dumper or watchdog activity.
            return false; // Cannot make the required measurement.
        }

        let out = self.reset_and_measure_sp();
        self.swd_finish();
        out.is_ok()
    }

    /// Resets the SP, forcing it to come up in debug halted state
    ///
    /// This is done per C1.4.1 in the ARMv7-M Architectural Reference Manual:
    /// - `DHCSR.C_DEBUGEN` enables halting debug
    /// - `DEMCR.VC_CORERESET` enables vector catch on the reset exception
    ///
    /// When the reset pin is released, we wait for a hard-coded timeout for
    /// `DFSR.HALTED` to be set to 1.
    ///
    /// If this function returns `Ok(())`, then the SP should be in debug halt;
    /// `DHCSR.C_DEBUGEN` and `DFSR.HALTED` should be set.
    ///
    /// If it returns an error, then the `DHCSR.C_DEBUGEN` bit is cleared, and
    /// the SP should be assumed to be running.
    ///
    /// In all cases, the reset line is not asserted when this function returns.
    /// Additionally, `DEMCR.VC_CORERESET` is always cleared, so future resets
    /// will not trigger a vector catch.
    #[allow(clippy::result_unit_err)]
    fn reset_into_debug_halt(&mut self) -> Result<(), ()> {
        // Asserting SP_RESET for >1ms here works.
        self.sp_reset_enter();
        hl::sleep_for(1);

        // Enable halting debug
        if self.dp_write_bitflags::<Dhcsr>(Dhcsr::resume()).is_err() {
            // If the write fails, then attempt to undo it
            ringbuf_entry!(Trace::DhcsrWriteError);
            self.disable_halting_debug();
            return Err(());
        }

        // Enable vector catch
        if self
            .dp_write_bitflags::<Demcr>(Demcr::VC_CORERESET)
            .is_err()
        {
            ringbuf_entry!(Trace::DemcrWriteError);
            self.disable_halting_debug();
            self.disable_vector_catch();
            return Err(());
        }
        self.sp_reset_leave();

        // 500ms max wait allows for testing using manual reset button.
        // Typical wait looks to be 5ms.
        let halted = self.wait_for_sp_halt(WAIT_FOR_HALT_MS);

        // We always disable vector catch, because we want the next reset to
        // proceed as usual.
        self.disable_vector_catch();
        let out = match halted {
            Ok(()) => {
                // Check that RESET was caught; if we halted for some other
                // reason (or can't read DFSR), then that's a bad sign.
                match self.dp_read_bitflags::<Dfsr>() {
                    Ok(dfsr) if dfsr.is_vcatch() => Ok(()),
                    Ok(dfsr) => {
                        ringbuf_entry!(Trace::Dfsr(dfsr));
                        ringbuf_entry!(Trace::VcCoreResetNotCaught);
                        Err(())
                    }
                    Err(_) => {
                        ringbuf_entry!(Trace::DfsrReadError);
                        Err(())
                    }
                }
            }
            Err(_) => Err(()),
        };
        if out.is_err() {
            self.disable_halting_debug();
        }
        out
    }

    /// Disables debug halt by clearing `DHCSR.C_DEBUGEN`
    fn disable_halting_debug(&mut self) {
        if self.dp_write_bitflags::<Dhcsr>(Dhcsr::end_debug()).is_err() {
            ringbuf_entry!(Trace::DhcsrWriteError);
        }
    }

    /// Disables vector catch by clearing `DEMCR.VC_CORERESET`
    fn disable_vector_catch(&mut self) {
        if self
            .dp_write_bitflags::<Demcr>(Demcr::from_bits_retain(0))
            .is_err()
        {
            ringbuf_entry!(Trace::DemcrWriteError);
        }
    }

    /// Resets the SP and obtains a measurement, sending it to `attest`
    ///
    /// Returns `Ok(())` if the SP was measured, `Err(())` if something went
    /// wrong and existing measurements should be invalidated.
    fn reset_and_measure_sp(&mut self) -> Result<(), ()> {
        self.reset_into_debug_halt()?;
        let success = match self.do_measure_sp() {
            Ok(digest) => {
                // SP resets the attestation log and record the new measurement.
                if self
                    .attest
                    .reset_and_record(HashAlgorithm::Sha3_256, &digest)
                    .is_ok()
                {
                    ringbuf_entry!(Trace::RecordedMeasurement);
                    Ok(())
                } else {
                    ringbuf_entry!(Trace::RecordMeasurementFailed);
                    Err(())
                }
            }
            Err(()) => {
                ringbuf_entry!(Trace::MeasureFailed);
                Err(())
            }
        };

        if success.is_ok() && self.reset_into_debug_halt().is_ok() {
            // Deposit the measurement token into SP memory
            if let Err(e) = self.write_single_target_addr(
                measurement_token::SP_ADDR as u32,
                measurement_token::VALID,
            ) {
                ringbuf_entry!(Trace::TokenWriteFail(e));
            }
            self.disable_halting_debug();
        } else {
            // Reset the SP into normal operation
            self.disable_halting_debug();
            self.sp_reset_enter();
            hl::sleep_for(1); // plenty of time, the internal pulse is 20µs
            self.sp_reset_leave();
        }
        success
    }
}

fn slice_to_le_u32(slice: &[u8]) -> Option<u32> {
    slice.try_into().map(u32::from_le_bytes).ok()
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = SYSCON.get_task_id();

    let gpio_task = GPIO.get_task_id();
    let attest = Attest::from(ATTEST.get_task_id());

    let mut spi = setup_spi(syscon);

    // This should correspond to SPI mode 0
    spi.initialize(
        device::spi0::cfg::MASTER_A::MASTER_MODE,
        device::spi0::cfg::LSBF_A::STANDARD, // MSB First
        device::spi0::cfg::CPHA_A::CHANGE,
        device::spi0::cfg::CPOL_A::LOW,
        spi_core::TxLvl::Tx7Items,
        spi_core::RxLvl::Rx1Item,
    );

    spi.enable();

    // Setup GPIO pins so that we can receive interrupts
    // and interact with the SP's SWD interface.
    let _ = setup_pins(gpio_task);

    // Check whether the dongle is connected at startup
    let gpio = drv_lpc55_gpio_api::Pins::from(gpio_task);
    let is_dongle_connected =
        gpio.read_val(SP_TO_ROT_JTAG_DETECT_L) == Value::Zero;
    if is_dongle_connected {
        switch_io_disconnected();
    }
    ringbuf_entry!(Trace::Start {
        is_dongle_connected
    });

    // Detect SP entering reset
    gpio.pint_enable(ROT_TO_SP_RESET_L_IN_PINT_SLOT, PintCondition::Falling);
    sys_irq_control(notifications::SP_RESET_IRQ_MASK, true);

    // JTAG active will block SWD operations.
    // We also need to detect JTAG going active so that if we've made a
    // measurement it can be invalidated.
    gpio.pint_enable(SP_TO_ROT_JTAG_DETECT_L_PINT_SLOT, PintCondition::Falling);
    sys_irq_control(notifications::JTAG_DETECT_IRQ_MASK, true);

    let mut server = ServerImpl {
        spi,
        gpio_task,
        gpio,
        attest,
        state: if is_dongle_connected {
            SwdState::Disconnected
        } else {
            SwdState::Connected { init: false }
        },
        transaction: None,
    };

    let mut incoming = [0; idl::INCOMING_SIZE];

    // TODO: If this is a power-on situation and SP and RoT are booting
    // at nearly the same time, can that be detected? That may be a
    // case where it is ok for the RoT to reset the SP and measure it.
    //
    // System power could be sequenced to allow the RoT
    // power to be on for 3-4 seconds before SP power on but that creates
    // a special case that complicates testing and building confidence

    // TODO: If SP is halted, then reset it.
    // It's conceivable that the RoT or this task could restart while the
    // SP is halted. Without code here to check for that case, it will be
    // necessary for an external party to take action through `ignition` or
    // other means to restart this system.
    //   - If this a whole RoT reboot
    //       - this is the normal no-worries case.
    //       - the attestation log will already be empty
    //   - else
    //       - this should never happen.
    //       - it would be good to surface this event so that it can be fixed.
    //       - attestation log should probably be reset.
    //   - attach to SP via SWD,
    //       - check for SP running/halted
    //       - if SP is halted or otherwise faulted
    //           - toggle SP RESET line.
    //           - RoT will be notified and will measure the SP
    //       - else
    //           - normal, no worries
    //           - We should get a clue to the SP or control plane that the SP needs to be
    //             measured. The control plane can trigger a measurement by asking the SP to
    //             reset itself. RoT detects that and takes a measurement.

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use drv_sp_ctrl_api::SpCtrlError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));
include!(concat!(env!("OUT_DIR"), "/swd.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/endoscope.rs"));
