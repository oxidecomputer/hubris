// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::convert::Into;
use drv_spi_api::{CsState, Spi};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use drv_update_api::{UpdateError, UpdateTarget};
use idol_runtime::{ClientError, Leased, RequestError, R, W};
use ringbuf::*;
use userlib::*;
#[cfg(feature = "sink_test")]
use zerocopy::{ByteOrder, LittleEndian};
// use serde::{Deserialize, Serialize};
// use hubpack::SerializedSize;

task_slot!(SPI, spi_driver);
task_slot!(SYS, sys);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    BadResponse(MsgType),
    BlockSize(usize),
    CSnAssert,
    CSnDeassert,
    Debug(bool),
    Error(SprotError),
    FailedRetries { retries: u16, errcode: SprotError },
    SprotError(SprotError),
    PulseFailed,
    RotNotReady,
    RotReadyTimeout,
    RxParseError(u8, u8, u8, u8),
    RxSpiError,
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
}
ringbuf!(Trace, 64, Trace::None);

const SP_TO_ROT_SPI_DEVICE: u8 = 0;

// TODO: These timeouts are somewhat arbitrary.
// All timeouts are in 'ticks'

/// Timeout for status message
const TIMEOUT_QUICK: u32 = 250;
/// Maximum timeout for an arbitrary message
const TIMEOUT_MAX: u32 = 500;
// XXX tune the RoT flash write timeout
const TIMEOUT_WRITE_ONE_BLOCK: u32 = 500;
// Delay between sending the portion of a message that fits entirely in the
// RoT's FIFO and the remainder of the message. This gives time for the RoT
// sprot task to respond to its interrupt.
const PART1_DELAY: u64 = 0;
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
            target_board = "sidecar-a",
            target_board = "sidecar-b",
            target_board = "psc-a",
            target_board = "psc-b",
            target_board = "gemini-bu-1"
            ))] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            // On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ
            // on pin2/PE3
            port: sys_api::Port::E,
            pin_mask: 1 << 3,
        };
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
            ).unwrap_lite();
            debug_set(sys, true);
        }

        fn debug_set(sys: &sys_api::Sys, asserted: bool) {
            ringbuf_entry!(Trace::Debug(asserted));
            sys.gpio_set_to(DEBUG_PIN, asserted).unwrap_lite();
        }
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

pub struct ServerImpl {
    sys: sys_api::Sys,
    spi: drv_spi_api::SpiDevice,
    // Use separate buffers so that retries can be generic.
    pub tx_buf: TxMsg,
    pub rx_buf: RxMsg,
}

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(SP_TO_ROT_SPI_DEVICE);
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::None)
        .unwrap_lite();
    debug_config(&sys);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        sys,
        spi,
        tx_buf: TxMsg::new(),
        rx_buf: RxMsg::new(),
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

impl ServerImpl {
    /// Handle the mechanics of sending a message and waiting for a response.
    fn do_send_recv(
        &mut self,
        txmsg: VerifiedTxMsg,
        timeout: u32,
    ) -> Result<VerifiedRxMsg, SprotError> {
        let size = txmsg.0;
        ringbuf_entry!(Trace::SendRecv(size));
        // Polling and timeout configuration
        // TODO: Use EXTI interrupt and just a timeout, no polling.

        // Assume that self.tx contains a valid message.

        if self.is_rot_irq_asserted() {
            ringbuf_entry!(Trace::UnexpectedRotIrq);
            // TODO: Move README.md to RFD 317 and discuss:
            //   - Unsolicited messages from RoT to SP.
            //   - Ignoring message from RoT to SP.
            //   - Should we send a message telling RoT that SP has booted?
            //
            // For now, we are surprised that ROT_IRQ is asserted
            // But it would be ok to overlap our new request with receiving
            // of a previous response.
            //
            // The RoT must be able to observe SP resets.
            // During the normal start-up seqeunce, the RoT is controlling the
            // SP's boot up sequence. However, the SP can reset itself and
            // individual Hubris tasks may fail and be restarted.
            //
            // If SP and RoT are out of sync, e.g. this task restarts and an old
            // response is still in the RoT's transmit FIFO, then we can also see
            // ROT_IRQ asserted when not expected.
            //
            // TODO: configuration parameters for delays below
            if !self.wait_rot_irq(false, TIMEOUT_QUICK) {
                ringbuf_entry!(Trace::UnexpectedRotIrq);
                if self.do_pulse_cs(10_u64, 10_u64)?.rot_irq_end == 1 {
                    ringbuf_entry!(Trace::PulseFailed);
                    // Did not clear ROT_IRQ
                    debug_set(&self.sys, false); // XXX
                    return Err(SprotError::RotNotReady);
                }
            }
        }
        let buf = &self.tx_buf.as_slice()[..size];

        // In order to improve reliability, start by sending only the
        // first ROT_FIFO_SIZE bytes and then delaying a short time.
        // If the RoT is ready, those first bytes will always fit
        // in the RoT receive FIFO. Eventually, the RoT FW will respond
        // to the interrupt and enter a tight loop to receive.
        // The short delay should cover most of the lag in RoT interrupt
        // handling.
        let part1 = if let Some(part1) = buf.get(0..ROT_FIFO_SIZE.min(size)) {
            part1
        } else {
            return Err(SprotError::BadMessageLength);
        };
        let part2 = buf.get(part1.len()..).unwrap_lite(); // empty or not
        ringbuf_entry!(Trace::TxPart1(part1.len()));
        ringbuf_entry!(Trace::TxPart2(part2.len()));
        if (PART1_DELAY != 0) || !part2.is_empty() {
            ringbuf_entry!(Trace::CSnAssert);
            self.spi
                .lock(CsState::Asserted)
                .map_err(|_| SprotError::SpiServerError)?;
            if PART1_DELAY != 0 {
                hl::sleep_for(PART1_DELAY);
            }
        }
        if self.spi.write(part1).is_err() {
            if (PART1_DELAY != 0) || !part2.is_empty() {
                ringbuf_entry!(Trace::CSnDeassert);
                _ = self.spi.release();
            }
            return Err(SprotError::SpiServerError);
        }
        if !part2.is_empty() {
            hl::sleep_for(PART2_DELAY); // TODO: configurable
            ringbuf_entry!(Trace::CSnDeassert);
            if self.spi.write(part2).is_err() {
                _ = self.spi.release();
                return Err(SprotError::SpiServerError);
            }
        }
        if ((PART1_DELAY != 0) || !part2.is_empty())
            && self.spi.release().is_err()
        {
            return Err(SprotError::SpiServerError);
        }

        /*
        // TODO: Use STM32 EXTI
        // STM32 EXTI allows for 16 interrupts for GPIOs.
        // Each of those can represent Pin X from a GPIO bank (A through K)
        // So, only one bank's Pin 3, for example, can have the #3 interrupt.
        // For ROT_IRQ, we would configure for the falling edge to trigger
        // the interrupt. That configuration should be specified in the app.toml
        // for the board. Work needs to be done to generalize the EXTI facility.
        // But, hacking in one interrupt as an example should be ok to start things
        // off.

        sys_irq_control(self.interrupt, true);
        // And wait for it to arrive.
        // TODO: There needs to be a timeout in case the RoT is out to lunch.
        let _rm =
        sys_recv_closed(&mut [], self.interrupt, TaskId::KERNEL)
        .unwrap_lite();
        */

        // We sleep and poll for what should be long enough for the RoT
        // to queue a response.
        // TODO: For better performance and power efficiency,
        // take an interrupt on ROT_IRQ falling edge with timeout.
        if !self.wait_rot_irq(true, timeout) {
            ringbuf_entry!(Trace::RotNotReady);
            return Err(SprotError::RotNotReady);
        }

        // Read just the header.
        // Keep CSn asserted over the two reads.
        ringbuf_entry!(Trace::CSnAssert);
        self.spi
            .lock(CsState::Asserted)
            .map_err(|_| SprotError::SpiServerError)?;
        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }

        // Fill in rx_buf with a complete message and validate its crc
        let res = self.do_read_response();

        // We must release the SPI bus before we return
        ringbuf_entry!(Trace::CSnDeassert);
        self.spi.release().map_err(|_| SprotError::SpiServerError)?;

        res
    }

    // Fetch as many bytes as we can and parse the header.
    // Return the parsed header or an error.
    //
    // We can fetch FIFO size number of bytes reliably.
    // After that, a short delay and fetch the rest if there is
    // a payload.
    // Small messages will fit entirely in the RoT FIFO.
    //
    // We don't, but we could speculate that some RoT responses will
    // be longer than ROT_FIFO_SIZE and get ROT_FIFO_SIZE
    // instead of MIN_MSG_SIZE.
    //
    // TODO: Use DMA on RoT to avoid this dance.
    //
    // We know statically that self.rx_buf is large enough to hold
    // part1_len bytes.
    fn do_read_response(&mut self) -> Result<VerifiedRxMsg, SprotError> {
        let part1_len = MIN_MSG_SIZE.min(ROT_FIFO_SIZE);
        ringbuf_entry!(Trace::RxPart1(part1_len));

        // We fill in all of buf or we fail
        let buf = &mut self.rx_buf.as_mut()[..part1_len];
        self.spi.read(buf).map_err(|_| {
            ringbuf_entry!(Trace::RxSpiError);
            SprotError::SpiServerError
        })?;

        let header = self.rx_buf.parse_header(part1_len).map_err(|e| {
            self.log_parse_error();
            e
        })?;

        let part2_len =
            MIN_MSG_SIZE + (header.payload_len as usize) - part1_len;
        ringbuf_entry!(Trace::RxPart2(part2_len));

        // Allow RoT time to rouse itself.
        hl::sleep_for(PART2_DELAY);

        // Read part two
        let buf = &mut self.rx_buf.as_mut()[part1_len..][..part2_len];
        self.spi.read(buf).map_err(|_| SprotError::SpiServerError)?;

        self.rx_buf.validate_crc(&header)?;

        Ok(VerifiedRxMsg(header))
    }

    fn log_parse_error(&self) {
        ringbuf_entry!(Trace::RxParseError(
            self.rx_buf.as_slice()[0],
            self.rx_buf.as_slice()[1],
            self.rx_buf.as_slice()[2],
            self.rx_buf.as_slice()[3]
        ));
    }

    fn do_send_recv_retries(
        &mut self,
        txmsg: VerifiedTxMsg,
        timeout: u32,
        retries: u16,
    ) -> Result<VerifiedRxMsg, SprotError> {
        let mut attempts_left = retries;
        let mut errcode = SprotError::Unknown;
        loop {
            if attempts_left == 0 {
                ringbuf_entry!(Trace::FailedRetries { retries, errcode });
                break;
            }
            attempts_left -= 1;

            match self.do_send_recv(txmsg, timeout) {
                // Recoverable errors dealing with our ability to receive
                // the message from the RoT.
                Err(err) => {
                    ringbuf_entry!(Trace::SprotError(err));
                    if is_recoverable_error(err) {
                        errcode = err;
                        continue;
                    } else {
                        return Err(err);
                    }
                }

                // Intact messages from the RoT may indicate an error on
                // its side.
                Ok(rxmsg) => {
                    match rxmsg.0.msgtype {
                        MsgType::ErrorRsp => {
                            if rxmsg.0.payload_len != 1 {
                                ringbuf_entry!(Trace::ErrRspPayloadSize(
                                    rxmsg.0.payload_len
                                ));
                                // Treat this as a recoverable error
                                continue;
                            }
                            let payload = &self.rx_buf.payload(&rxmsg);
                            errcode = SprotError::from(payload[0]);
                            ringbuf_entry!(Trace::SprotError(errcode));
                            if matches!(
                                errcode,
                                SprotError::FlowError
                                    | SprotError::InvalidCrc
                                    | SprotError::UnsupportedProtocol
                            ) {
                                // TODO: There are rare cases where
                                // the RoT dose not receive
                                // a 0x01 as the first byte in a message.
                                // See issue #929.
                                continue;
                            }
                            // Other errors from RoT are not recoverable with
                            // a retry.
                            return Err(errcode);
                        }
                        // All of the non-error message types are ok here.
                        _ => return Ok(rxmsg),
                    }
                }
            }
        }
        Err(errcode)
    }

    /// Clear the ROT_IRQ and the RoT's Tx buffer by toggling the CSn signal.
    /// ROT_IRQ before and after state is returned for testing.
    fn do_pulse_cs(
        &mut self,
        delay: u64,
        delay_after: u64,
    ) -> Result<PulseStatus, SprotError> {
        let rot_irq_begin = self.is_rot_irq_asserted();
        ringbuf_entry!(Trace::CSnAssert);
        self.spi
            .lock(CsState::Asserted)
            .map_err(|_| SprotError::CannotAssertCSn)?;
        if delay != 0 {
            hl::sleep_for(delay);
        }
        ringbuf_entry!(Trace::CSnDeassert);
        self.spi.release().unwrap_lite();
        if delay_after != 0 {
            hl::sleep_for(delay_after);
        }
        let rot_irq_end = self.is_rot_irq_asserted();
        let status = PulseStatus {
            rot_irq_begin: u8::from(rot_irq_begin),
            rot_irq_end: u8::from(rot_irq_end),
        };
        Ok(status)
    }

    fn is_rot_irq_asserted(&mut self) -> bool {
        self.sys.gpio_read(ROT_IRQ).unwrap_lite() == 0
    }

    // Poll ROT_IRQ until asserted (true) or deasserted (false).
    fn wait_rot_irq(&mut self, desired: bool, max_sleep: u32) -> bool {
        let mut slept = 0;
        while self.is_rot_irq_asserted() != desired {
            if slept == max_sleep {
                ringbuf_entry!(Trace::RotReadyTimeout);
                return false;
            }
            hl::sleep_for(1);
            slept += 1;
        }
        true
    }

    fn upd(
        &mut self,
        req: MsgType,
        payload_len: usize,
        rsp: MsgType,
        timeout: u32,
        attempts: u16,
    ) -> Result<Option<u32>, SprotError> {
        let txmsg = self.tx_buf.from_existing(req, payload_len)?;
        ringbuf_entry!(Trace::TxSize(txmsg.0));
        let rxmsg = self.do_send_recv_retries(txmsg, timeout, attempts)?;

        if rxmsg.0.msgtype == rsp {
            let rsp = self
                .rx_buf
                .deserialize_hubpack_payload::<UpdateRspHeader>(&rxmsg)?;
            ringbuf_entry!(Trace::UpdResponse(rsp));
            rsp.map_err(|e: u32| {
                UpdateError::try_from(e)
                    .unwrap_or(UpdateError::Unknown)
                    .into()
            })
        } else {
            expect_msg(MsgType::ErrorRsp, rxmsg.0.msgtype)?;
            if rxmsg.0.payload_len != 1 {
                return Err(SprotError::BadMessageLength);
            }
            let payload = self.rx_buf.payload(&rxmsg);
            let err = SprotError::from(payload[0]);
            ringbuf_entry!(Trace::Error(err));
            Err(err)
        }
    }
}

impl idl::InOrderSpRotImpl for ServerImpl {
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
        let txmsg = self.tx_buf.from_lease(msgtype, source)?;

        // Send message, then receive response using the same local buffer.
        match self.do_send_recv_retries(txmsg, TIMEOUT_MAX, attempts) {
            Ok(rxmsg) => {
                let payload = self.rx_buf.payload(&rxmsg);
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
    fn pulse_cs(
        &mut self,
        _: &RecvMessage,
        delay: u16,
    ) -> Result<PulseStatus, RequestError<SprotError>> {
        // If ROT_IRQ is asserted (a response is pending)
        // ROT_IRQ should be deasserted in response to CSn pulse.
        self.do_pulse_cs(delay.into(), delay.into())
            .map_err(|e| e.into())
    }

    cfg_if::cfg_if! {
        if #[cfg(feature = "sink_test")] {
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
            fn rot_sink(
                &mut self,
                _: &RecvMessage,
                count: u16,
                size: u16,
            ) -> Result<SinkStatus, RequestError<SprotError>> {
                let size = size as usize;
                debug_set(&self.sys, false);

                if size > core::mem::size_of::<u16>() {
                    // The payload is big enough to contain the sequence number
                    // and additional bytes.
                    let mut n: u8 = HEADER_SIZE as u8;
                    let buf = &mut self.tx_buf.payload_mut()[..size];
                    buf.fill_with(|| {
                        let seq = n;
                        n = n.wrapping_add(1);
                        seq
                    });
                }

                let mut sent = 0u16;
                let result = loop {
                    if sent == count {
                        break Ok(sent);
                    }
                    ringbuf_entry!(Trace::SinkLoop(sent));
                    // For debugging: Make sure each message is distinct.
                    // The first two payload bytes are a message
                    // sequence number if there is space for it.
                    if core::mem::size_of::<u16>() <= size {
                        let seq_buf = &mut self.tx_buf.payload_mut()[..core::mem::size_of::<u16>()];
                        LittleEndian::write_u16(seq_buf, sent);
                    }

                    match self.tx_buf.from_existing(MsgType::SinkReq, size) {
                        Err(_err) => break Err(SprotError::Serialization),
                        Ok(txmsg) => {
                            match self.do_send_recv_retries(txmsg, TIMEOUT_QUICK, MAX_SINKREQ_ATTEMPTS) {
                                Err(err) => {
                                    ringbuf_entry!(Trace::SinkFail(err, sent));
                                    break Err(err)
                                },
                                Ok(rxmsg ) => {
                                    match rxmsg.0.msgtype {
                                        MsgType::SinkRsp => {
                                            // TODO: Check sequence number in response.
                                            if rxmsg.0.payload_len as usize >= core::mem::size_of::<u16>() {
                                                let seq_buf = &self.rx_buf.payload(&rxmsg)[..core::mem::size_of::<u16>()];
                                                let r_seqno = LittleEndian::read_u16(seq_buf);
                                                if sent != r_seqno {
                                                    break Err(SprotError::Sequence);
                                                }
                                            }
                                        },
                                        MsgType::ErrorRsp => {
                                            let payload = self.rx_buf.payload(&rxmsg);
                                            if payload.len() != 1 {
                                                break Err(SprotError::BadMessageLength);
                                            }
                                            break Err(SprotError::from(payload[0]));
                                        },
                                        _ => {
                                            // Other non-SinkRsp messages from the RoT
                                            // are not recoverable with a retry.
                                            break Err(SprotError::BadMessageType);
                                        },
                                    }
                                },
                            }
                            sent = sent.wrapping_add(1);
                        },
                    }
                };
                debug_set(&self.sys, true);
                match result {
                    Ok(sent) => {
                        Ok(SinkStatus { sent })
                    },
                    Err(err) => {
                        Err(RequestError::Runtime(err))
                    },
                }
            }
        } else {
            fn rot_sink(
                &mut self,
                _: &RecvMessage,
                _count: u16,
                _size: u16,
            ) -> Result<SinkStatus, RequestError<SprotError>> {
                Err(RequestError::Runtime(SprotError::NotImplemented))
            }
        }
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
        let txmsg = self.tx_buf.no_payload(MsgType::StatusReq);
        let rxmsg = self.do_send_recv(txmsg, TIMEOUT_QUICK)?;
        expect_msg(MsgType::StatusRsp, rxmsg.0.msgtype)?;
        let status = self
            .rx_buf
            .deserialize_hubpack_payload::<SprotStatus>(&rxmsg)?;
        Ok(status)
    }

    fn io_stats(
        &mut self,
        _: &RecvMessage,
    ) -> Result<IoStats, RequestError<SprotError>> {
        let txmsg = self.tx_buf.no_payload(MsgType::IoStatsReq);
        let rxmsg = self.do_send_recv(txmsg, TIMEOUT_QUICK)?;
        expect_msg(MsgType::IoStatsRsp, rxmsg.0.msgtype)?;
        let status =
            self.rx_buf.deserialize_hubpack_payload::<IoStats>(&rxmsg)?;
        Ok(status)
    }

    fn block_size(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<SprotError>> {
        match self.upd(
            MsgType::UpdBlockSizeReq,
            0,
            MsgType::UpdBlockSizeRsp,
            TIMEOUT_QUICK,
            1,
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
        let payload = self.tx_buf.payload_mut();
        let payload_len = hubpack::serialize(&mut payload[0..], &image_type)
            .map_err(Into::<SprotError>::into)?;
        let _ = self.upd(
            MsgType::UpdPrepImageUpdateReq,
            payload_len,
            MsgType::UpdPrepImageUpdateRsp,
            TIMEOUT_QUICK,
            1,
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

        let payload = self.tx_buf.payload_mut();
        let n = hubpack::serialize(payload, &block_num)
            .map_err(|e| idol_runtime::RequestError::Runtime(e.into()))?;
        block
            .read_range(0..block.len(), &mut payload[n..n + block.len()])
            .map_err(|_| {
                idol_runtime::RequestError::Runtime(
                    SprotError::BadMessageLength,
                )
            })?;
        let payload_len = n + block.len();
        let _ = self.upd(
            MsgType::UpdWriteOneBlockReq,
            payload_len,
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
        let _ = self.upd(
            MsgType::UpdFinishImageUpdateReq,
            0,
            MsgType::UpdFinishImageUpdateRsp,
            TIMEOUT_QUICK,
            1,
        )?;
        Ok(())
    }

    fn abort_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let _ = self.upd(
            MsgType::UpdAbortUpdateReq,
            0,
            MsgType::UpdAbortUpdateRsp,
            TIMEOUT_QUICK,
            1,
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
