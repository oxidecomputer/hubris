// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::convert::Into;
use drv_spi_api::{CsState, SpiDevice, SpiServer};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use drv_update_api::{UpdateError, UpdateTarget};
use idol_runtime::{ClientError, Leased, RequestError, R, W};
use ringbuf::*;
use userlib::*;
#[cfg(feature = "sink_test")]
use zerocopy::{ByteOrder, LittleEndian};

cfg_if::cfg_if! {
    // Select local vs server SPI communication
    if #[cfg(feature = "use-spi-core")] {
        /// Claims the SPI core.
        ///
        /// This function can only be called once, and will panic otherwise!
        pub fn claim_spi(sys: &sys_api::Sys)
            -> drv_stm32h7_spi_server_core::SpiServerCore
        {
            drv_stm32h7_spi_server_core::declare_spi_core!(
                sys.clone(), notifications::SPI_IRQ_MASK)
        }
    } else {
        pub fn claim_spi(_sys: &sys_api::Sys) -> drv_spi_api::Spi {
            task_slot!(SPI, spi_driver);
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

task_slot!(SYS, sys);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    BadResponse(MsgType),
    BlockSize(usize),
    Debug(bool),
    Error(SprotError),
    FailedRetries { retries: u16, errcode: SprotError },
    SprotError(SprotError),
    PulseFailed,
    RotNotReady,
    RotReadyTimeout,
    RxParseError(u8, u8, u8, u8),
    RxSpiError(drv_spi_api::SpiError),
    RxPart1(usize),
    RxPart2(usize),
    SendRecv(usize),
    SinkFail(SprotError, u16),
    SinkLoop(u16),
    TxPart1(usize),
    TxPart2(usize),
    TxSize(usize),
    ErrRspPayloadSize(u16),
    UnexpectedRotIrq,
    UpdResponse(UpdateRspHeader),
    WrongMsgType(MsgType),
    UpdatePrep,
    UpdateWriteOneBlock,
    UpdateFinish,
    ErrRespNoPayload,
    Recoverable(SprotError),
    Header(MsgHeader),
    ErrWithHeader(SprotError, [u8; HEADER_SIZE]),
}
ringbuf!(Trace, 64, Trace::None);

// TODO:These timeouts are somewhat arbitrary.
// TODO: Make timeouts configurable
// All timeouts are in 'ticks'

/// Retry timeout for send_recv_retries
const RETRY_TIMEOUT: u64 = 100;

/// Timeout for status message
const TIMEOUT_QUICK: u32 = 250;
/// Default covers fail, pulse, retry
const DEFAULT_ATTEMPTS: u16 = 3;
/// Maximum timeout for an arbitrary message
const TIMEOUT_MAX: u32 = 500;
// XXX tune the RoT flash write timeout
const TIMEOUT_WRITE_ONE_BLOCK: u32 = 500;

// Delay between asserting CSn and sending the portion of a message
// that fits entierly in the RoT's FIFO.
const PART1_DELAY: u64 = 0;

// Delay between sending the portion of a message that fits entirely in the
// RoT's FIFO and the remainder of the message. This gives time for the RoT
// sprot task to respond to its interrupt.
const PART2_DELAY: u64 = 2; // Observed to be at least 2ms on gimletlet

const MAX_UPDATE_ATTEMPTS: u16 = 3;
cfg_if::cfg_if! {
    if #[cfg(feature = "sink_test")] {
        const MAX_SINKREQ_ATTEMPTS: u16 = 3; // TODO parameterize
    }
}

// ROT_IRQ comes from app.toml
// We use spi3 on gimletlet and spi4 on gemini and gimlet.
// You should be able to move the RoT board between SPI3, SPI4, and SPI6
// without much trouble even though SPI3 is the preferred connector and
// SPI4 is connected to the NET board.
cfg_if::cfg_if! {
    if #[cfg(any(
            target_board = "gimlet-b",
            target_board = "gimlet-c",
            target_board = "gimlet-d",
            target_board = "sidecar-b",
            target_board = "sidecar-c",
            target_board = "psc-a",
            target_board = "psc-b",
            target_board = "psc-c",
            target_board = "gemini-bu-1"
            ))] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            // On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ
            // on pin2/PE3
            port: sys_api::Port::E,
            pin_mask: 1 << 3,
        };
        const ROT_SPI_DEVICE: u8 = drv_spi_api::devices::ROT;
        fn debug_config(_sys: &sys_api::Sys) { }
        fn debug_set(_sys: &sys_api::Sys, _asserted: bool) { }
    } else if #[cfg(target_board = "gimletlet-2")] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            port: sys_api::Port::D,
            pin_mask: 1 << 0,
        };
        const DEBUG_PIN: sys_api::PinSet = sys_api::PinSet {
            port: sys_api::Port::E,
            pin_mask: 1 << 6,
        };
        fn debug_config(sys: &sys_api::Sys) {
            sys.gpio_configure_output(
                DEBUG_PIN,
                sys_api::OutputType::OpenDrain,
                sys_api::Speed::High,
                sys_api::Pull::Up
            );
            debug_set(sys, true);
        }

        fn debug_set(sys: &sys_api::Sys, asserted: bool) {
            ringbuf_entry!(Trace::Debug(asserted));
            sys.gpio_set_to(DEBUG_PIN, asserted);
        }
        const ROT_SPI_DEVICE: u8 = drv_spi_api::devices::SPI3_HEADER;
    } else {
        compile_error!("No configuration for ROT_IRQ");
    }
}

/// Return an error if the expected MsgType doesn't match the actual MsgType
fn expect_msg(expected: MsgType, actual: MsgType) -> Result<(), SprotError> {
    if expected != actual {
        ringbuf_entry!(Trace::WrongMsgType(actual));
        Err(SprotError::BadMessageType)
    } else {
        Ok(())
    }
}

pub struct Io<S: SpiServer> {
    stats: SpIoStats,
    sys: sys_api::Sys,
    spi: SpiDevice<S>,
}

pub struct ServerImpl<S: SpiServer> {
    io: Io<S>,
    tx_buf: [u8; BUF_SIZE],
    rx_buf: [u8; BUF_SIZE],
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys).device(ROT_SPI_DEVICE);

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::None);
    debug_config(&sys);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        io: Io {
            sys,
            spi,
            stats: SpIoStats::default(),
        },
        tx_buf: [0u8; BUF_SIZE],
        rx_buf: [0u8; BUF_SIZE],
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

// We separate out IO so that methods with retry loops don't have borrow checker problems due to
// tx_buf and rx_buf being borrowed from ServerImpl more than once.
impl<S: SpiServer> Io<S> {
    /// Handle the mechanics of sending a message and waiting for a response.
    ///
    /// We return success when `rxmsg` parses correctly, but we don't convert
    /// to `VerifiedRxMsg` and return that, because of borrow checker and
    /// ergonomic issues when this method is called in a loop. If we wanted
    /// to use a consuming `RxMsg::parse` we'd end up having to return the
    /// original `RxMsg` on errors so we could retry. This code looks much
    /// dirtier, as I noticed when I implemented it.
    pub fn do_send_recv<'a>(
        &mut self,
        txmsg: &VerifiedTxMsg<'a>,
        rxmsg: &mut RxMsg<'a>,
        timeout: u32,
    ) -> Result<MsgHeader, SprotError> {
        ringbuf_entry!(Trace::SendRecv(txmsg.len()));

        self.handle_rot_irq()?;
        self.do_send_request(txmsg)?;

        if !self.wait_rot_irq(true, timeout) {
            ringbuf_entry!(Trace::RotNotReady);
            return Err(SprotError::RotNotReady);
        }

        // Fill in rx_buf with a complete message and validate its crc
        self.do_read_response(rxmsg)
    }

    // Send a request in 2 parts, with optional delays before each part.
    //
    // In order to improve reliability, start by sending only the first
    // ROT_FIFO_SIZE bytes and then delaying a short time. If the RoT is ready,
    // those first bytes will always fit in the RoT receive FIFO. Eventually,
    // the RoT FW will respond to the interrupt and enter a tight loop to
    // receive. The short delay should cover most of the lag in RoT interrupt
    // handling.
    fn do_send_request(
        &mut self,
        msg: &VerifiedTxMsg,
    ) -> Result<(), SprotError> {
        // Increase the error count here. We'll decrease if we return successfully.
        self.stats.tx_errors = self.stats.tx_errors.wrapping_add(1);

        let part1_len = ROT_FIFO_SIZE.min(msg.len());
        let part1 = &msg.as_slice()[..part1_len];
        let part2 = &msg.as_slice()[part1_len..];
        ringbuf_entry!(Trace::TxPart1(part1.len()));
        ringbuf_entry!(Trace::TxPart2(part2.len()));

        let _lock = self.spi.lock_auto(CsState::Asserted)?;
        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }
        self.spi.write(part1)?;
        if !part2.is_empty() {
            hl::sleep_for(PART2_DELAY);
            self.spi.write(part2)?;
        }
        // Remove the error that we added at the beginning of this function
        self.stats.tx_errors = self.stats.tx_errors.wrapping_sub(1);
        self.stats.tx_sent = self.stats.tx_sent.wrapping_add(1);
        Ok(())
    }

    // Fetch as many bytes as we can and parse the header.
    // Return the parsed header or an error.
    //
    // We can fetch FIFO size number of bytes reliably.
    // After that, a short delay and fetch the rest if there is
    // a payload.
    // Small messages will fit entirely in the RoT FIFO.
    //
    // TODO: Use DMA on RoT to avoid this dance.
    //
    fn do_read_response<'a>(
        &mut self,
        rxmsg: &mut RxMsg<'a>,
    ) -> Result<MsgHeader, SprotError> {
        // Increase the error count here. We'll decrease if we return successfully.
        self.stats.rx_invalid = self.stats.rx_invalid.wrapping_add(1);

        // Disjoint borrow nonsense to satisfy the borrow checker
        let spi = &mut self.spi;

        let _lock = spi.lock_auto(CsState::Asserted)?;

        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }

        // If we read out the full fifo, we end up with an underrun situation
        // periodically. This happens after the part2 delay, and the length of
        // that delay doesn't matter. The interrupt fires quickly and we keep
        // looping waiting to read bytes. I noticed hundreds of iterations
        // without any data transferred with a ringbuf message inside the
        // tightloop in `Io::write_respsonse` on the RoT. Then all of a
        // sudden, the first data transfer occurs (~1 byte read/written) and
        // an underrun occurs. We seem to be able to prevent this leaving a
        // partially full TxBuf on the receiver. We want to retrieve a full
        // header, but other than that, we don't need to retrieve the full fifo
        // at once.
        //
        // In short, we set `part1_len = MIN_MSG_SIZE` instead of
        // `part1_len = ROT_FIFO_SIZE`.
        //
        // In the case that there is no payload, this still reads in one round.
        let part1_len = MIN_MSG_SIZE;
        ringbuf_entry!(Trace::RxPart1(part1_len));

        // Read part one
        rxmsg.read(part1_len, |buf| spi.read(buf).map_err(|e| e.into()))?;

        let header = match rxmsg.parse_header() {
            Ok(header) => header,
            Err(e) => {
                ringbuf_entry!(Trace::ErrWithHeader(e, rxmsg.header_bytes()));
                return Err(e);
            }
        };

        if part1_len < MIN_MSG_SIZE + (header.payload_len as usize) {
            // We haven't read the complete message yet.
            let part2_len =
                MIN_MSG_SIZE + (header.payload_len as usize) - part1_len;
            ringbuf_entry!(Trace::RxPart2(part2_len));

            // Allow RoT time to rouse itself.
            hl::sleep_for(PART2_DELAY);

            // Read part two
            rxmsg.read(part2_len, |buf| spi.read(buf).map_err(|e| e.into()))?;
        }

        ringbuf_entry!(Trace::Header(header));

        rxmsg.validate_crc(&header)?;
        self.stats.rx_invalid = self.stats.rx_invalid.wrapping_sub(1);
        self.stats.rx_received = self.stats.rx_received.wrapping_add(1);
        Ok(header)
    }

    fn do_send_recv_retries<'a>(
        &mut self,
        txmsg: &VerifiedTxMsg<'a>,
        rxmsg: &mut RxMsg<'a>,
        timeout: u32,
        retries: u16,
    ) -> Result<MsgHeader, SprotError> {
        let mut attempts_left = retries;
        let mut errcode = SprotError::Unknown;
        loop {
            if attempts_left == 0 {
                ringbuf_entry!(Trace::FailedRetries { retries, errcode });
                break;
            }

            if attempts_left != retries {
                self.stats.retries = self.stats.retries.wrapping_add(1);
                rxmsg.clear();
            }

            attempts_left -= 1;

            match self.do_send_recv(txmsg, rxmsg, timeout) {
                // Recoverable errors dealing with our ability to receive
                // the message from the RoT.
                Err(err) => {
                    ringbuf_entry!(Trace::SprotError(err));
                    if is_recoverable_error(err) {
                        errcode = err;
                        hl::sleep_for(RETRY_TIMEOUT);
                        continue;
                    } else {
                        return Err(err);
                    }
                }

                // Intact messages from the RoT may indicate an error on
                // its side.
                Ok(header) => {
                    match header.msgtype {
                        MsgType::ErrorRsp => {
                            self.stats.rx_errors =
                                self.stats.rx_errors.wrapping_add(1);
                            if header.payload_len != 1 {
                                ringbuf_entry!(Trace::ErrRspPayloadSize(
                                    header.payload_len
                                ));
                                // Treat this as a recoverable error
                                hl::sleep_for(RETRY_TIMEOUT);
                                ringbuf_entry!(Trace::ErrRespNoPayload);
                                continue;
                            }
                            errcode =
                                SprotError::from(rxmsg.payload_error_byte());
                            ringbuf_entry!(Trace::SprotError(errcode));
                            if is_recoverable_error(errcode) {
                                // TODO: There are rare cases where
                                // the RoT dose not receive
                                // a 0x01 as the first byte in a message.
                                // See issue #929.
                                hl::sleep_for(RETRY_TIMEOUT);
                                ringbuf_entry!(Trace::Recoverable(errcode));
                                continue;
                            }
                            // Other errors from RoT are not recoverable with
                            // a retry.
                            return Err(errcode);
                        }
                        // All of the non-error message types are ok here.
                        _ => return Ok(header),
                    }
                }
            }
        }
        Err(errcode)
    }

    // TODO: Move README.md to RFD 317 and discuss:
    //   - Unsolicited messages from RoT to SP.
    //   - Ignoring message from RoT to SP.
    //   - Should we send a message telling RoT that SP has booted?
    //
    // For now, we are surprised that ROT_IRQ is asserted.
    // But it would be ok to overlap our new request with receiving
    // of a previous response.
    //
    // TODO: The RoT must be able to observe SP resets. During the
    // normal start-up seqeunce, the RoT is controlling the SP's boot
    // up sequence. However, the SP can reset itself and individual
    // Hubris tasks may fail and be restarted.
    //
    // If SP and RoT are out of sync, e.g. this task restarts and an old
    // response is still in the RoT's transmit FIFO, then we can also see
    // ROT_IRQ asserted when not expected.
    //
    // TODO: configuration parameters for delays below
    fn handle_rot_irq(&mut self) -> Result<(), SprotError> {
        if self.is_rot_irq_asserted() {
            // See if the ROT_IRQ completes quickly.
            // This is the ROT_IRQ from the last request.
            if !self.wait_rot_irq(false, TIMEOUT_QUICK) {
                // Nope, it didn't complete. Pulse CSn.
                ringbuf_entry!(Trace::UnexpectedRotIrq);
                self.stats.csn_pulses += self.stats.csn_pulses.wrapping_add(1);
                // One sample of an LPC55S28 reacting to CSn deasserted
                // in about 54us. So, 10ms is plenty.
                if self.do_pulse_cs(10_u64, 10_u64)?.rot_irq_end == 1 {
                    // Did not clear ROT_IRQ
                    ringbuf_entry!(Trace::PulseFailed);
                    self.stats.csn_pulse_failures +=
                        self.stats.csn_pulse_failures.wrapping_add(1);
                    debug_set(&self.sys, false); // XXX
                    return Err(SprotError::RotNotReady);
                }
            }
        }
        Ok(())
    }

    /// Clear the ROT_IRQ and the RoT's Tx buffer by toggling the CSn signal.
    /// ROT_IRQ before and after state is returned for testing.
    fn do_pulse_cs(
        &self,
        assert_ms: u64,
        delay_ms_after: u64,
    ) -> Result<PulseStatus, SprotError> {
        let rot_irq_begin = self.is_rot_irq_asserted();
        let lock = self
            .spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| SprotError::CannotAssertCSn)?;
        if assert_ms != 0 {
            hl::sleep_for(assert_ms);
        }
        drop(lock);
        if delay_ms_after != 0 {
            hl::sleep_for(delay_ms_after);
        }
        let rot_irq_end = self.is_rot_irq_asserted();
        let status = PulseStatus {
            rot_irq_begin: u8::from(rot_irq_begin),
            rot_irq_end: u8::from(rot_irq_end),
        };
        Ok(status)
    }

    fn is_rot_irq_asserted(&self) -> bool {
        self.sys.gpio_read(ROT_IRQ) == 0
    }

    // Poll ROT_IRQ until asserted (true) or deasserted (false).
    //
    // We sleep and poll for what should be long enough for the RoT to queue
    // a response.
    //
    // TODO: Use STM32 EXTI as  an interrupt allows for better performance and
    // power efficiency.
    //
    // STM32 EXTI allows for 16 interrupts for GPIOs.
    // Each of those can represent Pin X from a GPIO bank (A through K)
    // So, only one bank's Pin 3, for example, can have the #3 interrupt.
    // For ROT_IRQ, we would configure for the falling edge to trigger
    // the interrupt. That configuration should be specified in the app.toml
    // for the board. Work needs to be done to generalize the EXTI facility.
    // But, hacking in one interrupt as an example should be ok to start things
    // off.
    fn wait_rot_irq(&mut self, desired: bool, max_sleep: u32) -> bool {
        let mut slept = 0;
        while self.is_rot_irq_asserted() != desired {
            if slept == max_sleep {
                self.stats.timeouts = self.stats.timeouts.wrapping_add(1);
                ringbuf_entry!(Trace::RotReadyTimeout);
                return false;
            }
            hl::sleep_for(1);
            slept += 1;
        }
        true
    }

    fn upd<'a>(
        &mut self,
        txmsg: &VerifiedTxMsg<'a>,
        mut rxmsg: RxMsg<'a>,
        rsp: MsgType,
        timeout: u32,
        attempts: u16,
    ) -> Result<Option<u32>, SprotError> {
        let header =
            self.do_send_recv_retries(txmsg, &mut rxmsg, timeout, attempts)?;

        expect_msg(rsp, header.msgtype)?;
        // The message must already have parsed successfully once. This
        // serves as an assertion against programmer error.  We can always
        // do a cheaper conversion without the redundant checks in the
        // future, if the cost of this proves prohibitive.
        let verified_rxmsg = rxmsg.parse().unwrap_lite();
        let rsp =
            verified_rxmsg.deserialize_hubpack_payload::<UpdateRspHeader>()?;
        ringbuf_entry!(Trace::UpdResponse(rsp));
        rsp.map_err(|e: u32| {
            UpdateError::try_from(e)
                .unwrap_or(UpdateError::Unknown)
                .into()
        })
    }
}

impl<S: SpiServer> idl::InOrderSpRotImpl for ServerImpl<S> {
    /// Send a message to the RoT for processing.
    fn send_recv(
        &mut self,
        recv_msg: &RecvMessage,
        msgtype: drv_sprot_api::MsgType,
        source: Leased<R, [u8]>,
        sink: Leased<W, [u8]>,
    ) -> Result<Received, RequestError<SprotError>> {
        self.send_recv_retries(recv_msg, msgtype, 1, source, sink)
    }

    /// Send a message to the RoT for processing.
    fn send_recv_retries(
        &mut self,
        _: &RecvMessage,
        msgtype: drv_sprot_api::MsgType,
        attempts: u16,
        source: Leased<R, [u8]>,
        sink: Leased<W, [u8]>,
    ) -> Result<Received, RequestError<SprotError>> {
        let txmsg = TxMsg::new(&mut self.tx_buf[..]);
        let verified_txmsg = txmsg.from_lease(msgtype, source)?;
        let mut rxmsg = RxMsg::new(&mut self.rx_buf[..]);

        match self.io.do_send_recv_retries(
            &verified_txmsg,
            &mut rxmsg,
            TIMEOUT_MAX,
            attempts,
        ) {
            Ok(_) => {
                self.io.stats.tx_sent = self.io.stats.tx_sent.wrapping_add(1);
                let verified_rxmsg = rxmsg.parse().unwrap_lite();
                let payload = verified_rxmsg.payload();
                if !payload.is_empty() {
                    sink.write_range(0..payload.len(), payload).map_err(
                        |_| RequestError::Fail(ClientError::WentAway),
                    )?;
                }
                Ok(Received {
                    length: u16::try_from(payload.len()).unwrap_lite(),
                    msgtype: msgtype as u8,
                })
            }
            Err(err) => Err(idol_runtime::RequestError::Runtime(err)),
        }
    }

    /// Clear the RoT Tx buffer and have the RoT deassert ROT_IRQ.
    /// The status of ROT_IRQ before and after the assert is returned.
    ///
    /// If ROT_IRQ is asserted (a response is pending)
    /// ROT_IRQ should be deasserted in response to CSn pulse.
    fn pulse_cs(
        &mut self,
        _: &RecvMessage,
        delay: u16,
    ) -> Result<PulseStatus, RequestError<SprotError>> {
        self.io
            .do_pulse_cs(delay.into(), delay.into())
            .map_err(|e| e.into())
    }

    /// Send `count` buffers of `size` size to simulate a firmare
    /// update or other bulk data transfer from the SP to the RoT.
    //
    // The RoT will read all of the bytes of a MsgType::SinkReq and
    // include the received sequence number in its SinkRsp message.
    //
    // The RoT reports errors in an ErrorRsp message.
    //
    // For the sake of working with a logic analyzer,
    // a known pattern is put into the SinkReq messages so that
    // most of the received bytes match their buffer index modulo
    // 0x100.
    //
    #[cfg(feature = "sink_test")]
    fn rot_sink(
        &mut self,
        _: &RecvMessage,
        count: u16,
        size: u16,
    ) -> Result<SinkStatus, RequestError<SprotError>> {
        let size = size as usize;
        debug_set(&self.io.sys, false);

        let mut txmsg = TxMsg::new(&mut self.tx_buf[..]);
        let mut rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let mut sent = 0u16;
        let result = loop {
            if sent == count {
                break Ok(sent);
            }
            ringbuf_entry!(Trace::SinkLoop(sent));

            match txmsg.sink_req(size, sent) {
                Err(err) => break Err(err),
                Ok(verified_txmsg) => {
                    match self.io.do_send_recv_retries(
                        &verified_txmsg,
                        &mut rxmsg,
                        TIMEOUT_QUICK,
                        MAX_SINKREQ_ATTEMPTS,
                    ) {
                        Err(err) => {
                            ringbuf_entry!(Trace::SinkFail(err, sent));
                            break Err(err);
                        }
                        Ok(header) => {
                            // A succesful return from `do_send_recv_retries`
                            // indicates parsing already succeeded once.
                            let verified_rxmsg = rxmsg.parse().unwrap_lite();
                            match header.msgtype {
                                MsgType::SinkRsp => {
                                    // TODO: Check sequence number in response.
                                    if verified_rxmsg.payload().len() >= 2 {
                                        let seq_buf =
                                            &verified_rxmsg.payload()[..2];
                                        let r_seqno =
                                            LittleEndian::read_u16(seq_buf);
                                        if sent != r_seqno {
                                            break Err(SprotError::Sequence);
                                        }
                                        rxmsg = verified_rxmsg.into_rxmsg();
                                    } else {
                                        // We only allow sending payloads of 2 bytes or more.
                                        break Err(
                                            SprotError::BadMessageLength,
                                        );
                                    }
                                }
                                MsgType::ErrorRsp => {
                                    if verified_rxmsg.payload().len() != 1 {
                                        break Err(
                                            SprotError::BadMessageLength,
                                        );
                                    }
                                    break Err(SprotError::from(
                                        verified_rxmsg.payload()[0],
                                    ));
                                }
                                _ => {
                                    // Other non-SinkRsp messages from the RoT
                                    // are not recoverable with a retry.
                                    break Err(SprotError::BadMessageType);
                                }
                            }
                        }
                    }
                    sent = sent.wrapping_add(1);
                    txmsg = verified_txmsg.into_txmsg();
                }
            }
        };

        debug_set(&self.io.sys, true);
        match result {
            Ok(sent) => Ok(SinkStatus { sent }),
            Err(err) => Err(RequestError::Runtime(err)),
        }
    }

    #[cfg(not(feature = "sink_test"))]
    fn rot_sink(
        &mut self,
        _: &RecvMessage,
        _count: u16,
        _size: u16,
    ) -> Result<SinkStatus, RequestError<SprotError>> {
        Err(RequestError::Runtime(SprotError::NotImplemented))
    }

    /// Retrieve status from the RoT.
    ///
    /// Use trusted interfaces when available. This is meant as
    /// an early or fallback source of information prior to stronger
    /// levels of trust being established.
    /// Having a signed StatusRsp is possible, but consider that carefully.
    fn status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<SprotStatus, RequestError<SprotError>> {
        let txmsg =
            TxMsg::new(&mut self.tx_buf[..]).no_payload(MsgType::StatusReq);
        let mut rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let header = self.io.do_send_recv_retries(
            &txmsg,
            &mut rxmsg,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        expect_msg(MsgType::StatusRsp, header.msgtype)?;
        let status = rxmsg
            .parse()
            .unwrap_lite()
            .deserialize_hubpack_payload::<SprotStatus>()?;
        Ok(status)
    }

    fn io_stats(
        &mut self,
        _: &RecvMessage,
    ) -> Result<IoStats, RequestError<SprotError>> {
        let txmsg =
            TxMsg::new(&mut self.tx_buf[..]).no_payload(MsgType::IoStatsReq);
        let mut rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let header = self.io.do_send_recv_retries(
            &txmsg,
            &mut rxmsg,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        expect_msg(MsgType::IoStatsRsp, header.msgtype)?;
        let rot_stats = rxmsg
            .parse()
            .unwrap_lite()
            .deserialize_hubpack_payload::<RotIoStats>()?;
        Ok(IoStats {
            rot: rot_stats,
            sp: self.io.stats,
        })
    }

    fn block_size(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<SprotError>> {
        let txmsg = TxMsg::new(&mut self.tx_buf[..])
            .no_payload(MsgType::UpdBlockSizeReq);
        let rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        match self.io.upd(
            &txmsg,
            rxmsg,
            MsgType::UpdBlockSizeRsp,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )? {
            Some(block_size) => {
                let bs = block_size as usize;
                ringbuf_entry!(Trace::BlockSize(bs));
                Ok(bs)
            }
            None => Err(idol_runtime::RequestError::Runtime(
                SprotError::UpdateSpRotError,
            )),
        }
    }

    fn prep_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
        image_type: UpdateTarget,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        ringbuf_entry!(Trace::UpdatePrep);
        let txmsg = TxMsg::new(&mut self.tx_buf[..])
            .serialize(MsgType::UpdPrepImageUpdateReq, image_type)
            .map_err(|(_, e)| SprotError::from(e))?;
        let rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let _ = self.io.upd(
            &txmsg,
            rxmsg,
            MsgType::UpdPrepImageUpdateRsp,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        Ok(())
    }

    fn write_one_block(
        &mut self,
        _msg: &userlib::RecvMessage,
        block_num: u32,
        // XXX Is a separate length needed here? Lease always 1024 even if not all used?
        // XXX 1024 needs to come from somewhere.
        block: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            1024,
        >,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        ringbuf_entry!(Trace::UpdateWriteOneBlock);
        let txmsg = TxMsg::new(&mut self.tx_buf[..]).block(block_num, block)?;
        let rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let _ = self.io.upd(
            &txmsg,
            rxmsg,
            MsgType::UpdWriteOneBlockRsp,
            TIMEOUT_WRITE_ONE_BLOCK,
            MAX_UPDATE_ATTEMPTS,
        )?;
        Ok(())
    }

    fn finish_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let txmsg = TxMsg::new(&mut self.tx_buf[..])
            .no_payload(MsgType::UpdFinishImageUpdateReq);
        let rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let _ = self.io.upd(
            &txmsg,
            rxmsg,
            MsgType::UpdFinishImageUpdateRsp,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        Ok(())
    }

    fn abort_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let txmsg = TxMsg::new(&mut self.tx_buf[..])
            .no_payload(MsgType::UpdAbortUpdateReq);
        let rxmsg = RxMsg::new(&mut self.rx_buf[..]);
        let _ = self.io.upd(
            &txmsg,
            rxmsg,
            MsgType::UpdAbortUpdateRsp,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        Ok(())
    }
}

mod idl {
    use super::{
        IoStats, MsgType, PulseStatus, Received, SinkStatus, SprotError,
        SprotStatus, UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
