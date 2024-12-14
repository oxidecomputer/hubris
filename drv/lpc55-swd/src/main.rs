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
//      the read but this is aborbed into idle cycles.
//
// - The SWD protocol is LSB first. This works very well when bit-banging but
//   somewhat less well with a register based hardware block such as SPI. The
//   SPI controller can do LSB first transfers but it turns out to be easier to
//   debug and understand if we keep it in MSB form and reverse bits where
//   needed. Endianness is one of the hardest problems in programming after
//   all.

#![no_std]
#![no_main]

use attest_api::{Attest, AttestError, HashAlgorithm};
use drv_lpc55_gpio_api::{Direction, Pins, Value};
use drv_lpc55_spi as spi_core;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use drv_sp_ctrl_api::SpCtrlError;
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use lpc55_pac as device;
use ringbuf::*;
use sha3::{Digest, Sha3_256};
use userlib::{
    hl, set_timer_relative, sys_get_timer, sys_irq_control, sys_set_timer,
    task_slot, RecvMessage, TaskId, UnwrapLite,
};
use zerocopy::AsBytes;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Idcode(u32),
    Idr(u32),
    MemVal(u32),
    // SwdRead(Port, RawSwdReg),
    ReadCmd,
    WriteCmd,
    None,
    AckErr(Ack),
    DongleDetected,
    Dhcsr(u32),
    ReadDhcsr(u32),
    ReadDemcr(u32),
    WriteDhcsr(u32),
    WriteDemcr(u32),
    WriteOther(u32),
    ParityFail { data: u32, received_parity: u16 },
    EnabledWatchdog,
    DisabledWatchdog,
    WatchdogFired,
    WatchdogSwap(Result<(), Ack>),

    // TODO: Remove most of the following before merge.
    AttestError(AttestError),
    BlockReadError(usize, usize),
    CaptureSpBoot,
    DbResetSpBegin,
    DbResetSpEnd,
    DoHalt,
    DoSetup,
    EndOfNotificationHandler,
    EnterSpReset,
    HaltFail(u32),
    HaltRequest,
    HaltWait,
    Halted(u32),
    InvalidateFailed(AttestError),
    InvalidateSpMeasurement,
    LeaveSpReset(bool),
    LeftReset,
    LeftSpReset,
    Line,
    MeasureSp,
    MeasuredSp { ok: bool, delta_t: u32 },
    NeedSwdInit,
    PinSetupDefaults,
    RecordedInvalidMeasurement,
    ResumeFail,
    Resumed,
    SpJtagDetectFired,
    SpResetAsserted,
    SpResetFired,
    SpResetNotAsserted,
    StartHash(u32),
    SwdSetupFail,
    SwdSetupOk,
    Timeout,
    TimerHandlerError(SpCtrlError),
    PardonMe,
    Bad,
    ReadX { start: u32, len: u32 },
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
const CSW_HPROT: u32 = 0x0b000000;

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

// Some DHCSR bits have different read vs. write meanings
const DHCSR: u32 = 0xE000EDF0;
const DHCSR_HALT_MAGIC: u32 = DHCSR_DBGKEY + DHCSR_C_HALT + DHCSR_C_DEBUGEN;
const DHCSR_DEBUG_MAGIC: u32 = DHCSR_DBGKEY + DHCSR_C_DEBUGEN;
const DHCSR_RESUME_MAGIC: u32 = DHCSR_DBGKEY;
const DHCSR_RESUME_W_DEBUG_MAGIC: u32 = DHCSR_DBGKEY + DHCSR_C_DEBUGEN;
const DHCSR_S_HALT: u32 = 1 << 17;
const DHCSR_S_REGRDY: u32 = 1 << 16;

const DHCSR_DBGKEY: u32 = 0xA05F << 16;
const DHCSR_C_HALT: u32 = 1 << 1;
const DHCSR_C_DEBUGEN: u32 = 1 << 0;

const DCRSR: u32 = 0xE000EDF4;
const DCRDR: u32 = 0xE000EDF8;

const DEMCR: u32 = 0xE000EDFC;
const DEMCR_MON_EN: u32 = 1 << 16;
const DEMCR_VC_CORERESET: u32 = 1 << 0;

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

struct ServerImpl {
    spi: spi_core::Spi,
    gpio: TaskId,
    attest: Attest,
    init: bool,
    transaction: Option<MemTransaction>,
    watchdog_ms: Option<u32>,
}

impl idl::InOrderSpCtrlImpl for ServerImpl {
    fn read_transaction_start(
        &mut self,
        _: &RecvMessage,
        start: u32,
        end: u32,
    ) -> Result<(), RequestError<SpCtrlError>> {
        if !self.init {
            return Err(SpCtrlError::NeedInit.into());
        }
        ringbuf_entry!(Trace::ReadX {
            start,
            len: end - start
        });
        self.start_read_transaction(start, ((end - start) as usize) / 4)
            .map_err(|_| SpCtrlError::Fault.into())
    }

    fn read_transaction(
        &mut self,
        _: &RecvMessage,
        dest: LenLimit<Leased<W, [u8]>, 4096>,
    ) -> Result<(), RequestError<SpCtrlError>> {
        if !self.init {
            return Err(SpCtrlError::NeedInit.into());
        }

        let cnt = dest.len();
        if cnt % 4 != 0 {
            return Err(SpCtrlError::BadLen.into());
        }
        let mut buf = LeaseBufWriter::<_, 32>::from(dest.into_inner());

        for _ in 0..cnt / 4 {
            match self.read_transaction_word() {
                Ok(r) => {
                    if let Some(w) = r {
                        ringbuf_entry!(Trace::MemVal(w));
                        for b in w.to_le_bytes() {
                            if buf.write(b).is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(_) => return Err(SpCtrlError::Fault.into()),
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
        if !self.init {
            return Err(SpCtrlError::NeedInit.into());
        }
        let cnt = dest.len();
        if cnt % 4 != 0 {
            return Err(SpCtrlError::BadLen.into());
        }
        let mut buf = LeaseBufWriter::<_, 32>::from(dest.into_inner());

        for i in 0..cnt / 4 {
            match self.read_single_target_addr(addr + ((i * 4) as u32)) {
                Ok(r) => {
                    ringbuf_entry!(Trace::MemVal(r));
                    for b in r.to_le_bytes() {
                        if buf.write(b).is_err() {
                            return Ok(());
                        }
                    }
                }
                Err(_) => return Err(SpCtrlError::Fault.into()),
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
        if !self.init {
            return Err(SpCtrlError::NeedInit.into());
        }
        let cnt = dest.len();
        if cnt % 4 != 0 {
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
            if self
                .write_single_target_addr(
                    addr + ((i * 4) as u32),
                    u32::from_le_bytes(word),
                )
                .is_err()
            {
                return Err(SpCtrlError::Fault.into());
            }
        }

        Ok(())
    }

    fn setup(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        match self.do_setup_swd() {
            Ok(()) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn halt(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        match self.do_halt() {
            Ok(()) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn resume(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SpCtrlError>> {
        match self.do_resume() {
            Ok(()) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn read_core_register(
        &mut self,
        _: &RecvMessage,
        register: u16,
    ) -> Result<u32, RequestError<SpCtrlError>> {
        // C1.6 Debug system registers
        let r = match register {
            // R0-R12
            0b0000000..=0b0001100

            // LR - PSP
            | 0b0001101..=0b0010010

            // CONTROL/FAULTMASK/BASEPRI/PRIMASK
            | 0b0010100

            // FPCSR
            | 0b0100001

            // S0-S31
            | 0b1000000..=0b1011111 => Ok::<u16, SpCtrlError>(register),
            _ => Err(SpCtrlError::InvalidCoreRegister)
        }?;

        if self.write_single_target_addr(DCRSR, r as u32).is_err() {
            return Err(SpCtrlError::Fault.into());
        }

        loop {
            match self.read_single_target_addr(DHCSR) {
                Ok(dhcsr) => {
                    ringbuf_entry!(Trace::Dhcsr(dhcsr));

                    if dhcsr & DHCSR_S_REGRDY != 0 {
                        break;
                    }
                }
                Err(_) => {
                    return Err(SpCtrlError::Fault.into());
                }
            }
        }

        match self.read_single_target_addr(DCRDR) {
            Ok(val) => Ok(val),
            Err(_) => Err(SpCtrlError::Fault.into()),
        }
    }

    fn enable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
        time_ms: u32,
    ) -> Result<(), RequestError<SpCtrlError>> {
        ringbuf_entry!(Trace::EnabledWatchdog);
        if !self.init {
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
        // That will kick off an SP image measurement which takes about 0.8s.
        //
        // At the time of writing this comment, the watchdog timer value used
        // in omicron is 2000ms. We're using a relatively big chunk of that
        // time (~40%) measuring the SP.
        //
        // TODO: Test around possible negative interactions during update.
        //   If SP on any of the platforms takes more than 1.2 seconds, that
        //   needs to be accommodated.
        self.watchdog_ms = Some(time_ms);
        Ok(())
    }

    fn disable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Trace::DisabledWatchdog);
        self.watchdog_ms = None;
        sys_set_timer(None, notifications::TIMER_MASK);
        Ok(())
    }

    /// Normally, the measurement is implicitly triggered by SP reset.
    /// TODO: Remove this debugging support,
    fn db_measure_sp(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<[u8; 32], RequestError<SpCtrlError>> {
        if self.do_setup_swd().is_ok() {
            ringbuf_entry!(Trace::Line);
            self.do_halt().unwrap();
        } else {
            return Err(RequestError::Runtime(SpCtrlError::Fault));
        }
        let _ = self.write_single_target_addr(DHCSR, DHCSR_HALT_MAGIC);
        let digest = self.do_measure_sp();
        let _ = self.write_single_target_addr(DEMCR, 0);
        let _ = self.write_single_target_addr(DHCSR, DHCSR_RESUME_MAGIC);
        self.do_release_swd();
        digest.map_err(|_| RequestError::Runtime(SpCtrlError::Fault))
    }

    /// Remote debugging support.
    /// Yet another way to reset the SP. This one is known to finish before
    /// any other code in this task runs.
    /// TODO: remove this function
    fn db_reset_sp(
        &mut self,
        _msg: &userlib::RecvMessage,
        delay: u32,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        ringbuf_entry!(Trace::DbResetSpBegin);
        self.sp_reset_enter(); // XXX may need to clear self.transaction if dumper is active.
        hl::sleep_for(delay.into());
        // Don't clean up interrupt state, we want to trigger a measurement.
        let _ = self.swd_setup();
        let _ = self.write_single_target_addr(DEMCR, 0);
        let _ = self.write_single_target_addr(DHCSR, DHCSR_RESUME_MAGIC);
        self.swd_finish();
        self.sp_reset_leave(false);
        self.init = false;
        ringbuf_entry!(Trace::DbResetSpEnd);
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
            + notifications::SP_RESET_IRQ_MASK
            + notifications::JTAG_DETECT_IRQ_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        // If JTAG_DETECT fires:
        //   - invalidate any SP measurement
        //   - if still asserted, then the other handlers will fail on
        //     their calls to do_setup_swd();
        //   - We could try extending the Watchdog timer if it is active in hopes
        //     that the SP dongle is removed or its power is removed, but if
        //     there is a dongle attached to the SP, then let the humans figure it out
        //     and don't complicate the behavior here.
        //

        let mut invalidate = false;
        let gpio = Pins::from(self.gpio);

        if (bits & notifications::JTAG_DETECT_IRQ_MASK) != 0 {
            ringbuf_entry!(Trace::SpJtagDetectFired);
            const SLOT: PintSlot = SP_TO_ROT_JTAG_DETECT_L_PINT_SLOT;
            if gpio
                .pint_op(SLOT, PintOp::Detected, PintCondition::Falling)
                .map_or(false, |v| v.unwrap_lite())
            {
                ringbuf_entry!(Trace::InvalidateSpMeasurement);
                invalidate = true;
                self.init = false;
            }
            let _ = gpio.pint_op(SLOT, PintOp::Clear, PintCondition::Status);
            sys_irq_control(notifications::JTAG_DETECT_IRQ_MASK, true);
        }

        if (bits & notifications::TIMER_MASK) != 0 {
            ringbuf_entry!(Trace::WatchdogFired);

            match self.do_setup_swd() {
                Ok(()) => {
                    // Disable the watchdog timer
                    sys_set_timer(None, notifications::TIMER_MASK);
                    // Attempt to do the swap
                    let r = self.swap_sp_slot();
                    ringbuf_entry!(Trace::WatchdogSwap(r));

                    // Force reinitialization
                    self.init = false;
                }
                Err(e) => {
                    // This should only fail if JTAG_DETECT or SP_RESET are currently asserted.
                    ringbuf_entry!(Trace::TimerHandlerError(e));
                }
            }
            self.watchdog_ms = None;
        }

        if (bits & notifications::SP_RESET_IRQ_MASK) != 0 {
            ringbuf_entry!(Trace::SpResetFired);
            if !invalidate && !self.do_handle_sp_reset() {
                ringbuf_entry!(Trace::InvalidateSpMeasurement);
                invalidate = true;
            }
            // else something something {
            //  We are not going to try to measure/trust the SP
            //  when there is a glitch on the JTAG_DETECT signal.
            //
            //  e.g. JTAG_DETECT fired but before the handler was called, it
            //  deasserted so that the SP_RESET that also fired could be
            //  handled successfully.
            //}

            // TODO: Get rid of spurious interrupts cause by do_handle_sp_reset()
            // toggling SP_RESET.
            // There could be a "real" SP_RESET during or since the handler
            // started.

            const SLOT: PintSlot = ROT_TO_SP_RESET_L_IN_PINT_SLOT;
            let _ = gpio.pint_op(SLOT, PintOp::Clear, PintCondition::Status);
            sys_irq_control(notifications::SP_RESET_IRQ_MASK, true);
            ringbuf_entry!(Trace::EndOfNotificationHandler);
        }

        if invalidate {
            ringbuf_entry!(Trace::InvalidateSpMeasurement);
            if let Err(e) = self.invalidate_sp_measurement() {
                // TODO: recovery needed.
                // We don't know the current state of the SP and
                // we were not able to update the attestation log.
                ringbuf_entry!(Trace::InvalidateFailed(e));
            }
        }
    }
}

impl ServerImpl {
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
        let rounded = ((cnt + 7) / 8) * 8;
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
        let parity: u32 = u32::from(val.count_ones() % 2 != 0);

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
        // ringbuf_entry!(Trace::SwdRead(port, reg));
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
        let gpio = Pins::from(self.gpio);
        gpio.read_val(SP_TO_ROT_JTAG_DETECT_L) == Value::Zero
    }

    fn swd_setup(&mut self) -> Result<(), Ack> {
        self.io_out();
        // Section B5.2.2 of ADIv6 specifies this sequence
        self.reset();
        self.swd_switch();
        self.reset();

        self.idle_cycles(16);

        // Must read DP IDCODE register after reset
        let result =
            self.swd_read(Port::DP, RawSwdReg::DpRead(DpRead::IDCode))?;

        ringbuf_entry!(Trace::Idcode(result));

        self.power_up()?;

        // Read the IDR as a basic test for reading from the AP
        let result = self.swd_read_ap_reg(ApAddr(0, ApReg::IDR), false)?;
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

        match addr {
            DHCSR => ringbuf_entry!(Trace::ReadDhcsr(val)),
            DEMCR => ringbuf_entry!(Trace::ReadDemcr(val)),
            _ => (), // ringbuf_entry!(Trace::ReadOther(addr, val)),
        }
        Ok(val)
    }

    fn write_single_target_addr(
        &mut self,
        addr: u32,
        val: u32,
    ) -> Result<(), Ack> {
        if self.transaction.is_some() {
            return Err(Ack::Fault);
        }

        match addr {
            DHCSR => ringbuf_entry!(Trace::WriteDhcsr(val)),
            DEMCR => ringbuf_entry!(Trace::WriteDemcr(val)),
            _ => ringbuf_entry!(Trace::WriteOther(val)),
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

    fn pin_setup(&mut self) {
        setup_pins(self.gpio).unwrap_lite();
    }

    /// Swaps the currently-active SP slot
    fn swap_sp_slot(&mut self) -> Result<(), Ack> {
        // All registers and constants are within the FLASH peripheral block, so
        // I'm going to skip prefixing everything with `FLASH_`.

        // RM0433 Table 8
        const BASE: u32 = 0x52002000;

        // RM0433 Section 4
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
        self.sp_reset_leave(true);
    }

    fn sp_reset_enter(&mut self) {
        setup_rot_to_sp_reset_l_out(self.gpio);
        let gpio = Pins::from(self.gpio);
        ringbuf_entry!(Trace::EnterSpReset);
        gpio.set_val(ROT_TO_SP_RESET_L_OUT, Value::Zero);
    }

    fn sp_reset_leave(&mut self, cleanup: bool) {
        ringbuf_entry!(Trace::LeaveSpReset(cleanup));
        let gpio = Pins::from(self.gpio);
        setup_rot_to_sp_reset_l_in(self.gpio);
        gpio.set_val(ROT_TO_SP_RESET_L_IN, Value::One); // should be a no-op
        if cleanup {
            let _ = gpio.pint_op(
                ROT_TO_SP_RESET_L_IN_PINT_SLOT,
                PintOp::Clear,
                PintCondition::Rising,
            );
            let _ = gpio.pint_op(
                ROT_TO_SP_RESET_L_IN_PINT_SLOT,
                PintOp::Clear,
                PintCondition::Falling,
            );
        }
        ringbuf_entry!(Trace::LeftSpReset);
        // XXX setup interrupts?
    }

    fn do_measure_sp(&mut self) -> Result<[u8; 32], ()> {
        ringbuf_entry!(Trace::MeasureSp);
        let start = sys_get_timer().now;
        // TODO: Guard against performing measurements so close together that the SP cannot get any work done.
        // How much time is that?
        // XXX const MIN_TIME_BETWEEN_MEASUREMENTS: usize = 5 * 60 * 1000;

        // Stuff we know about SP images but would rather get
        // from somewhere else:
        mod sp {
            pub const IMAGE_ADDR: u32 = 0x0800_0000;
            pub const END_ADDR: u32 = 0x0810_1000;
            pub const HEADER_ADDR: u32 = IMAGE_ADDR + 0x298;
            pub const MAGIC_ADDR: u32 = HEADER_ADDR + 0;
            pub const IMAGELENGTH_ADDR: u32 = HEADER_ADDR + 4;
            pub const MIN_IMAGE_SIZE: usize = 0x10000; // An arbitrary minimum
            pub const BANK_SIZE: usize = (END_ADDR - IMAGE_ADDR) as usize;
        }

        const READ_SIZE: usize = 256;
        const SP_IMAGE_BLOCKS: usize = sp::BANK_SIZE / READ_SIZE;

        // For Hubris on the STM32H7, we have FWID padding go to the end
        // of the flash bank. We need to read an entire 1MiB to calculate
        // a valid FWID.
        // With current image sizes we could save time by reading
        // only the image and then use local 0xff bytes to extend the hash.
        // However, to meet our security goals, we need to measure all of
        // the bytes.
        // A sample of SP image sizes shows that we
        // psc:        flash:   0x74100 (45%)
        // grapefruit: flash:   0x81b00 (50%)
        // gimlet      flash:   0xa0900 (62%)
        // sidecar:    flash:   0xaa100 (66%)

        let mut buf: [u8; READ_SIZE] = [0; READ_SIZE];
        let mut magic = 0;
        let mut total_image_size = 0;

        if self
            .read_buf_from_addr(sp::MAGIC_ADDR, magic.as_bytes_mut())
            .is_ok()
        {
            if magic == userlib::HEADER_MAGIC {
                if self
                    .read_buf_from_addr(
                        sp::IMAGELENGTH_ADDR,
                        total_image_size.as_bytes_mut(),
                    )
                    .is_ok()
                {
                    if (sp::MIN_IMAGE_SIZE..sp::BANK_SIZE)
                        .contains(&(total_image_size as usize))
                    {
                        ringbuf_entry!(Trace::StartHash(total_image_size));
                        // XXX does this help?
                        // ringbuf_entry!(Trace::Line);
                        //if self.do_setup_swd().is_ok() {
                        //    ringbuf_entry!(Trace::Line);
                        //    let _ = self
                        //        .write_single_target_addr(DHCSR, DHCSR_HALT_MAGIC)
                        //        .map_err(|_| SpCtrlError::Fault);
                        //}
                        let _ = self.read_single_target_addr(DHCSR);
                        let _ = self.read_single_target_addr(DEMCR);
                        let mut hash = Sha3_256::new();
                        let mut bytes_hashed = 0usize;
                        // Measure the entire bank to match the expected FWID.
                        for index in 0..SP_IMAGE_BLOCKS {
                            if self
                                .read_buf_from_addr(
                                    (index * READ_SIZE) as u32 + sp::IMAGE_ADDR,
                                    &mut buf,
                                )
                                .is_err()
                            {
                                ringbuf_entry!(Trace::BlockReadError(
                                    index,
                                    bytes_hashed
                                ));
                                return Err(());
                            }
                            // accumulate the hash
                            hash.update(&buf[..]);
                            bytes_hashed += buf.len();
                        }
                        let now = sys_get_timer().now;
                        ringbuf_entry!(Trace::MeasuredSp {
                            ok: true,
                            delta_t: (now - start) as u32
                        });
                        Ok(hash.finalize().into())
                    } else {
                        ringbuf_entry!(Trace::Bad);
                        Err(())
                    }
                } else {
                    ringbuf_entry!(Trace::Bad);
                    Err(())
                }
            } else {
                ringbuf_entry!(Trace::Bad);
                Err(())
            }
        } else {
            ringbuf_entry!(Trace::Bad);
            Err(())
        }
    }

    /*
    fn read_buf_from_addr(
        &mut self,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), Ack> {
        let words = buf.len() / 4;
        let mut addr = addr;
        let mut offset = 0;
        let mut remain = buf.len();
        for _ in 0..words {
            if remain == 0 {
                break;
            }
            let r = self.read_single_target_addr(addr)?;
            for b in r.to_le_bytes() {
                if remain > 0 {
                    buf[offset] = b;
                    offset += 1;
                    remain -= 1;
                } else {
                    break;
                }
            }
            addr += 4;
        }
        Ok(())
    }
    */

    fn read_buf_from_addr(
        &mut self,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), SpCtrlError> {
        let start = addr;
        let end = addr + buf.len() as u32;
        self.start_read_transaction(start, ((end - start) as usize) / 4)
            .map_err(|_| SpCtrlError::Fault)?;

        let cnt = buf.len();
        if cnt % 4 != 0 {
            return Err(SpCtrlError::BadLen);
        }

        let mut i = 0usize;
        for _ in 0..cnt / 4 {
            match self.read_transaction_word() {
                Ok(r) => {
                    if let Some(w) = r {
                        // ringbuf_entry!(Trace::MemVal(w));
                        for b in w.to_le_bytes() {
                            buf[i] = b;
                            i += 1;
                        }
                    }
                }
                Err(_) => return Err(SpCtrlError::Fault),
            }
        }

        Ok(())
    }

    fn do_setup_swd(&mut self) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::DoSetup);

        // TODO: clients interferring with each other and SP_RESET handling.
        //
        // A client can call into this SWD task using multiple API calls.
        // While that session is active, inbetween calls, an SP_INTERRUPT
        // can happen that will change the state of the SWD interface and the
        // SP itself. If there were multiple external clients (hiffy vs dump),
        // they could interfere with each other's work.
        //
        // The dump client is trying to be non-intrusive and will try to resume
        // the SP after finishing its work, the last call being `resume` or, if
        // that fails, `setup` followed by `resume`.
        // TODO: Client state or a session ID could be maintained.
        // The TaskId of the current client (includeing task swd's TaskID) can be
        // tested and an error returned if there is not a match.
        // Calls to setup (or do_setup) would change the stored client TaskId.
        if !self.init {
            ringbuf_entry!(Trace::PinSetupDefaults);
            // This should be redundant since setup_pins needs to be
            // called in order for SP_RESET interrupt to be plumbed.
            // The only exception is if SP_RESET is driven as an output
            // that that should be a non-interruptable transient state.
            self.pin_setup();
        }

        if self.swd_dongle_detected() {
            ringbuf_entry!(Trace::DongleDetected);
            return Err(SpCtrlError::DongleDetected);
        }

        match self.swd_setup() {
            Ok(_) => {
                ringbuf_entry!(Trace::SwdSetupOk);
                self.init = true;
                Ok(())
            }
            Err(_) => {
                ringbuf_entry!(Trace::SwdSetupFail);
                Err(SpCtrlError::Fault)
            }
        }
    }

    fn do_release_swd(&mut self) {}

    fn do_halt(&mut self) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::DoHalt);
        self.halt_request()?;
        self.halt_wait(5000)
    }

    fn halt_request(&mut self) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::HaltRequest);
        let _ = self.read_single_target_addr(DHCSR);
        let r = self
            .write_single_target_addr(DHCSR, DHCSR_HALT_MAGIC)
            .map_err(|_| SpCtrlError::Fault);
        let _ = self.read_single_target_addr(DHCSR);
        r
    }

    fn halt_wait(&mut self, timeout: u64) -> Result<(), SpCtrlError> {
        ringbuf_entry!(Trace::HaltWait);
        let start = sys_get_timer().now;
        let deadline = start.wrapping_add(timeout);
        loop {
            match self.read_single_target_addr(DHCSR) {
                Ok(dhcsr) => {
                    ringbuf_entry!(Trace::Dhcsr(dhcsr));
                    if dhcsr & DHCSR_S_HALT != 0 {
                        ringbuf_entry!(Trace::Halted(
                            (sys_get_timer().now - start) as u32
                        ));
                        return Ok(());
                    }
                }
                Err(_) => {
                    ringbuf_entry!(Trace::HaltFail(
                        (sys_get_timer().now - start) as u32
                    ));
                    return Err(SpCtrlError::Fault);
                }
            }
            if deadline <= sys_get_timer().now {
                ringbuf_entry!(Trace::Timeout);
                break Err(SpCtrlError::Timeout);
            }
            hl::sleep_for(1);
        }
    }

    fn do_resume(&mut self) -> Result<(), SpCtrlError> {
        match self.write_single_target_addr(DHCSR, DHCSR_RESUME_W_DEBUG_MAGIC) {
            Ok(_) => {
                ringbuf_entry!(Trace::Resumed);
                Ok(())
            }
            Err(_) => {
                ringbuf_entry!(Trace::ResumeFail);
                Err(SpCtrlError::Fault)
            }
        }
    }

    fn invalidate_sp_measurement(&mut self) -> Result<(), AttestError> {
        // TODO: Attest task needs an API for this.
        // let invalid_measurement = [0xffu8; 32];
        let invalid_measurement = b"<<Invalid SP Measurement Hash_>>";
        match self
            .attest
            .record(HashAlgorithm::Sha3_256, invalid_measurement)
        {
            Ok(()) => {
                ringbuf_entry!(Trace::RecordedInvalidMeasurement);
                Ok(())
            }
            // We should reboot RoT if log is full.
            // But really, there should be an assigned slot for the
            // SP measurement.
            Err(AttestError::LogFull) => {
                ringbuf_entry!(Trace::AttestError(AttestError::LogFull));
                Err(AttestError::LogFull)
            }
            // XXX Some programmer error.
            // Not possible, don't reboot.
            // We should record some state and
            // there needs to be a new release to fix it.
            Err(e) => {
                ringbuf_entry!(Trace::AttestError(e));
                Err(e)
            }
        }
    }

    // Return false if measurement was needed but not successful.
    fn do_handle_sp_reset(&mut self) -> bool {
        const UNDO_SWD: u32 = 1 << 0; // Need self.swd_finish()
        const UNDO_RESET: u32 = 1 << 1; // Need self.sp_reset_leave(true)
        const UNDO_VC_CORERESET: u32 = 1 << 2; // Need DEMCR = 0
        const UNDO_DEBUGEN: u32 = 1 << 3; // Need DHCSR = DHCSR_RESUME_MAGIC
        let start = sys_get_timer().now;
        let gpio = Pins::from(self.gpio);
        const SLOT: PintSlot = ROT_TO_SP_RESET_L_IN_PINT_SLOT;
        let mut need_undo = 0u32;

        // Did SP_RESET transition to Zero?
        if !gpio
            .pint_op(SLOT, PintOp::Detected, PintCondition::Falling)
            .map_or(false, |v| v.unwrap_lite())
        {
            ringbuf_entry!(Trace::SpResetNotAsserted);
            // This is a "spurious" intrerrupt that can probably be eliminated.
            // Cases where we assert SP_RESET then clean-up the PINT condition will
            // still have a pending notification.
            // TODO: clean that up.
            return true; // no work required.
        }

        // A reset happened. If we don't get a measurement then
        // make sure that the old one is invalidated.
        ringbuf_entry!(Trace::SpResetAsserted);

        // This notification handler should be compatible with watchdog but
        // will result in the invalidation of any dumps held in the SP if successful.
        // TODO: confirm that bank-flipping SP update watchdog is working.
        if !self.init {
            ringbuf_entry!(Trace::NeedSwdInit);
            if self.do_setup_swd().is_err() {
                ringbuf_entry!(Trace::Line);
                return false; // Cannot make the required measurement.
            }
            need_undo = UNDO_SWD;
        } else {
            // We may have interrupted dumper or watchdog activity.
            ringbuf_entry!(Trace::PardonMe);
        }

        // Armv7-M Arch Ref:
        // C1.4.1 Entering Debug state on leaving reset state
        //
        // To force the processor to enter Debug state as soon as it
        // comes out of reset, a debugger sets DHCSR.C_DEBUGEN to 1, to
        // enable Halting debug, and sets DEMCR.VC_CORERESET to 1 to
        // enable vector catch on the Reset exception. When the
        // processor comes out of reset it sets DHCSR.C_HALT to 1,
        // and enters Debug state.

        // If we are late to the party, we're not that late.
        // In any case, keep/force the SP into a reset condition.
        // Though AIRCR::SYSRESETREQ can be used to effect a local reset.
        // that does not necessarily reset the whole SP SoC.
        // So, use the SP_RESET GPIO.

        ringbuf_entry!(Trace::Line);
        self.sp_reset_enter();
        need_undo += UNDO_RESET;
        hl::sleep_for(5); // XXX What is the minimum assertion time for SP RESET?

        let mut ok = false;
        if self
            .write_single_target_addr(DHCSR, DHCSR_DEBUG_MAGIC)
            .is_ok()
        {
            need_undo += UNDO_DEBUGEN;
            ringbuf_entry!(Trace::CaptureSpBoot);
            if self
                .write_single_target_addr(
                    DEMCR,
                    DEMCR_MON_EN + DEMCR_VC_CORERESET,
                )
                .is_ok()
            {
                ringbuf_entry!(Trace::Line);
                need_undo += UNDO_VC_CORERESET;
                self.sp_reset_leave(true);
                need_undo &= !UNDO_RESET;
                ringbuf_entry!(Trace::LeftReset);
                if self.halt_wait(100).is_ok() {
                    ringbuf_entry!(Trace::Line);
                    if let Ok(digest) = self.do_measure_sp() {
                        ringbuf_entry!(Trace::Line);
                        if self
                            .attest
                            .record(HashAlgorithm::Sha3_256, &digest)
                            .is_ok()
                        {
                            ok = true;
                            ringbuf_entry!(Trace::Line);
                        } else {
                            ringbuf_entry!(Trace::Bad);
                        }
                        if self
                            .write_single_target_addr(DHCSR, DHCSR_RESUME_MAGIC)
                            .is_ok()
                        {
                            ringbuf_entry!(Trace::Line);
                        } else {
                            ringbuf_entry!(Trace::Bad);
                        }
                    } else {
                        ringbuf_entry!(Trace::Bad);
                    }
                } else {
                    ringbuf_entry!(Trace::Bad);
                }
            } else {
                ringbuf_entry!(Trace::Bad);
            }
        } else {
            ringbuf_entry!(Trace::Bad);
        }
        if need_undo & UNDO_VC_CORERESET != 0 {
            let _ = self.write_single_target_addr(DEMCR, 0);
            ringbuf_entry!(Trace::Line);
        }
        if need_undo & UNDO_DEBUGEN != 0 {
            let _ = self.write_single_target_addr(DHCSR, DHCSR_RESUME_MAGIC);
            ringbuf_entry!(Trace::Line);
        }
        if need_undo & UNDO_RESET != 0 {
            self.sp_reset_leave(true);
            ringbuf_entry!(Trace::Line);
        }

        // TODO: Make sure tht the SP is actually running here.

        if need_undo & UNDO_SWD != 0 {
            self.swd_finish();
            ringbuf_entry!(Trace::Line);
        }

        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::MeasuredSp {
            ok,
            delta_t: (now - start) as u32
        });
        ok
    }
}

#[export_name = "main"]
fn main() -> ! {
    let syscon = SYSCON.get_task_id();

    let gpio = GPIO.get_task_id();
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

    let mut server = ServerImpl {
        spi,
        gpio,
        attest,
        init: false,
        transaction: None,
        watchdog_ms: None,
    };

    // Setup GPIO pins so that we can receive interrupts.
    server.pin_setup();

    // Detect SP entering reset
    let _ = Pins::from(server.gpio).pint_op(
        ROT_TO_SP_RESET_L_IN_PINT_SLOT,
        PintOp::Enable,
        PintCondition::Falling,
    );
    sys_irq_control(notifications::SP_RESET_IRQ_MASK, true);

    // JTAG active will block SWD operations.
    // We also need to detect JTAG going active so that if we've made a
    // measurement it can be invalidated.
    let _ = Pins::from(server.gpio).pint_op(
        SP_TO_ROT_JTAG_DETECT_L_PINT_SLOT,
        PintOp::Enable,
        PintCondition::Falling,
    );
    sys_irq_control(notifications::JTAG_DETECT_IRQ_MASK, true);

    let mut incoming = [0; idl::INCOMING_SIZE];
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
