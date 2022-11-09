// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_spi_api::{CsState, Spi};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use drv_update_api::{ImageVersion, UpdateError, UpdateTarget};
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
    Error(MsgError),
    FailedRetries { retries: u16, errcode: MsgError },
    MsgError(MsgError),
    ParseErr(u8, u8, u8, u8),
    PulseFailed,
    RotNotReady,
    RotReadyTimeout,
    RxPart1(usize),
    RxPart2(usize),
    RxPayloadRemainingMutErr(u8, u8, u8, u8),
    SendRecv(usize),
    SinkFail(MsgError, u16),
    SinkLoop(u16),
    TxPart1(usize),
    TxPart2(usize),
    TxSize(usize),
    UnexpectedRotIrq,
    UpdResponse(UpdateRspHeader),
    WrongMsgType(MsgType),
}
ringbuf!(Trace, 64, Trace::None);

const SP_TO_ROT_SPI_DEVICE: u8 = 0;

// TODO: These timeouts are somewhat arbitrary.

/// Timeout for status message
const TIMEOUT_QUICK: u32 = 1000;
/// Maximum timeout for an arbitrary message
const TIMEOUT_MAX: u32 = 2_000;
// XXX tune the RoT flash write timeout
const TIMEOUT_WRITE_ONE_BLOCK: u32 = 2_000;
// Delay between sending the portion of a message that fits entirely in the
// RoT's FIFO and the remainder of the message. This gives time for the RoT
// sprot task to respond to its interrupt.
const PART1_DELAY: u64 = 0;
const PART2_DELAY: u64 = 2; // Observed to be at least 2ms on gimletlet

const MAX_UPD_ATTEMPTS: u16 = 3;
cfg_if::cfg_if! {
    if #[cfg(feature = "sink_test")] {
        const MAX_SINKREQ_ATTEMPTS: u16 = 2; // TODO parameterize
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

pub struct ServerImpl {
    sys: sys_api::Sys,
    spi: drv_spi_api::SpiDevice,
    // Use separate buffers so that retries can be generic.
    pub tx_buf: [u8; BUF_SIZE],
    pub rx_buf: [u8; BUF_SIZE],
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
        tx_buf: [0u8; BUF_SIZE],
        rx_buf: [0u8; BUF_SIZE],
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

impl ServerImpl {
    /// Handle the mechanics of sending a message and waiting for a response.
    fn do_send_recv(
        &mut self,
        size: usize,
        timeout: u32,
    ) -> Result<(MsgType, usize), MsgError> {
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
                    return Err(MsgError::RotNotReady);
                }
            }
        }
        let buf = match self.tx_buf.get(0..size) {
            Some(buf) => buf,
            None => {
                return Err(MsgError::BadMessageLength);
            }
        };

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
            return Err(MsgError::BadMessageLength);
        };
        let part2 = buf.get(part1.len()..).unwrap_lite(); // empty or not
        ringbuf_entry!(Trace::TxPart1(part1.len()));
        ringbuf_entry!(Trace::TxPart2(part2.len()));
        if (PART1_DELAY != 0) || !part2.is_empty() {
            ringbuf_entry!(Trace::CSnAssert);
            self.spi
                .lock(CsState::Asserted)
                .map_err(|_| MsgError::SpiServerError)?;
            if PART1_DELAY != 0 {
                hl::sleep_for(PART1_DELAY);
            }
        }
        if self.spi.write(part1).is_err() {
            if (PART1_DELAY != 0) || !part2.is_empty() {
                ringbuf_entry!(Trace::CSnDeassert);
                _ = self.spi.release();
            }
            return Err(MsgError::SpiServerError);
        }
        if !part2.is_empty() {
            hl::sleep_for(PART2_DELAY); // TODO: configurable
            ringbuf_entry!(Trace::CSnDeassert);
            if self.spi.write(part2).is_err() {
                _ = self.spi.release();
                return Err(MsgError::SpiServerError);
            }
        }
        if ((PART1_DELAY != 0) || !part2.is_empty())
            && self.spi.release().is_err()
        {
            return Err(MsgError::SpiServerError);
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
            return Err(MsgError::RotNotReady);
        }

        // Read just the header.
        // Keep CSn asserted over the two reads.
        ringbuf_entry!(Trace::CSnAssert);
        self.spi
            .lock(CsState::Asserted)
            .map_err(|_| MsgError::SpiServerError)?;
        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }

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
        let part1_len = MIN_MSG_SIZE.min(ROT_FIFO_SIZE);
        ringbuf_entry!(Trace::RxPart1(part1_len));
        let buf = self.rx_buf.get_mut(..part1_len).unwrap_lite();
        let result: Result<usize, MsgError> = if self.spi.read(buf).is_err() {
            Err(MsgError::SpiServerError)
        } else {
            match rx_payload_remaining_mut(part1_len, &mut self.rx_buf) {
                Ok(buf) => {
                    ringbuf_entry!(Trace::RxPart2(buf.len()));
                    // Allow RoT time to rouse itself.
                    hl::sleep_for(PART2_DELAY);
                    if self.spi.read(buf).is_err() {
                        Err(MsgError::SpiServerError)
                    } else {
                        Ok(part1_len + buf.len())
                    }
                }
                Err(err) => {
                    ringbuf_entry!(Trace::RxPayloadRemainingMutErr(
                        self.rx_buf[0],
                        self.rx_buf[1],
                        self.rx_buf[2],
                        self.rx_buf[3]
                    ));
                    Err(err)
                }
            }
        };

        ringbuf_entry!(Trace::CSnDeassert);
        if self.spi.release().is_err() {
            Err(MsgError::SpiServerError)
        } else {
            match result {
                Err(e) => {
                    ringbuf_entry!(Trace::ParseErr(
                        self.rx_buf[0],
                        self.rx_buf[1],
                        self.rx_buf[2],
                        self.rx_buf[3]
                    ));
                    Err(e)
                }
                Ok(rlen) => match parse(&self.rx_buf[0..rlen]) {
                    Err(e) => {
                        ringbuf_entry!(Trace::ParseErr(
                            self.rx_buf[0],
                            self.rx_buf[1],
                            self.rx_buf[2],
                            self.rx_buf[3]
                        ));
                        Err(e)
                    }
                    Ok((msgtype, payload_buf)) => {
                        Ok((msgtype, payload_buf.len()))
                    }
                },
            }
        }
    }

    fn do_send_recv_retries(
        &mut self,
        size: usize,
        timeout: u32,
        retries: u16,
    ) -> Result<(MsgType, usize), MsgError> {
        let mut attempts_left = retries;
        let mut errcode = MsgError::Unknown;
        loop {
            if attempts_left == 0 {
                ringbuf_entry!(Trace::FailedRetries { retries, errcode });
                break;
            }
            attempts_left -= 1;

            match self.do_send_recv(size, timeout) {
                // Recoverable errors dealing with our ability to receive
                // the message from the RoT.
                Err(err) if err == MsgError::InvalidCrc => {
                    errcode = err;
                    continue;
                }
                Err(err)
                    if matches!(
                        err,
                        MsgError::EmptyMessage
                            | MsgError::RotNotReady
                            | MsgError::RotBusy
                    ) =>
                {
                    errcode = err;
                    continue;
                }
                // The remaining errors are not recoverable.
                Err(err) => {
                    ringbuf_entry!(Trace::MsgError(err));
                    errcode = err;
                    break;
                }
                // Intact messages from the RoT may indicate an error on
                // its side.
                Ok((msgtype, payload_len)) => {
                    match msgtype {
                        MsgType::ErrorRsp if payload_len > 0 => {
                            let payload = payload_buf(
                                Some(payload_len),
                                &self.rx_buf[..],
                            );
                            errcode =
                                MsgError::from_u8(payload[0]).unwrap_lite();
                            ringbuf_entry!(Trace::MsgError(errcode));
                            if matches!(
                                errcode,
                                MsgError::FlowError
                                    | MsgError::InvalidCrc
                                    | MsgError::UnsupportedProtocol
                            ) {
                                // TODO: There are rare cases where
                                // the RoT dose not receive
                                // a 0x01 as the first byte in a message.
                                // See issue XXX.
                                continue;
                            }
                            // Other codes from RoT are not recoverable
                            // with a retry.
                            break;
                        }
                        MsgType::ErrorRsp => {
                            // No optional error code present.
                            errcode = MsgError::Unknown;
                            break;
                        }
                        // All of the non-error message types are ok here.
                        _ => return Ok((msgtype, payload_len)),
                    }
                }
            }
        }
        Err(errcode)
    }

    /// Retrieve low-level RoT status
    fn do_status(&mut self) -> Result<Status, MsgError> {
        match compose(
            MsgType::StatusReq,
            0,
            self.tx_buf.get_mut(..).unwrap_lite(),
        ) {
            Ok(size) => match self.do_send_recv(size, TIMEOUT_QUICK) {
                Err(err) => Err(err),
                Ok((msgtype, payload_size)) => match msgtype {
                    MsgType::StatusRsp => {
                        match hubpack::deserialize::<Status>(payload_buf(
                            Some(payload_size),
                            &self.rx_buf[..],
                        )) {
                            Ok((status, _n)) => Ok(status),
                            Err(_) => Err(MsgError::Serialization),
                        }
                    }
                    _ => {
                        ringbuf_entry!(Trace::BadResponse(msgtype));
                        Err(MsgError::BadResponse)
                    }
                },
            },
            Err(_err) => Err(MsgError::Serialization),
        }
    }

    /// Clear the ROT_IRQ and the RoT's Tx buffer by toggling the CSn signal.
    /// ROT_IRQ before and after state is returned for testing.
    fn do_pulse_cs(
        &mut self,
        delay: u64,
        delay_after: u64,
    ) -> Result<PulseStatus, MsgError> {
        let rot_irq_begin = self.is_rot_irq_asserted();
        ringbuf_entry!(Trace::CSnAssert);
        self.spi
            .lock(CsState::Asserted)
            .map_err(|_| MsgError::CannotAssertCSn)?;
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
    ) -> Result<Option<u32>, RequestError<UpdateError>> {
        let size =
            compose(req, payload_len, &mut self.tx_buf[..]).map_err(|_| {
                idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
            })?;
        ringbuf_entry!(Trace::TxSize(size));
        let (msgtype, payload_len) = self
            .do_send_recv_retries(size, timeout, attempts)
            .map_err(|_| {
                idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
            })?;
        if msgtype == rsp {
            let buf = payload_buf(Some(payload_len), &self.rx_buf[..]);
            let (rsp, _) = hubpack::deserialize::<UpdateRspHeader>(buf)
                .map_err(|_| {
                    idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
                })?;
            ringbuf_entry!(Trace::UpdResponse(rsp));
            match rsp.kind {
                UpdateRspKind::Ok => Ok(None),
                UpdateRspKind::Error => {
                    let u_err = if let Ok(u_err) =
                        drv_update_api::UpdateError::try_from(rsp.value as u8)
                    {
                        u_err
                    } else {
                        drv_update_api::UpdateError::Unknown
                    };
                    Err(RequestError::Runtime(u_err))
                }
                UpdateRspKind::Value => Ok(Some(rsp.value)),
                _ => Err(RequestError::Runtime(UpdateError::SpRotError)),
            }
        } else {
            match msgtype {
                MsgType::ErrorRsp if (payload_len > 0) => {
                    ringbuf_entry!(Trace::Error(MsgError::from(
                        payload_buf(None, &self.rx_buf[..])[0]
                    )));
                    Err(idol_runtime::RequestError::Runtime(
                        UpdateError::SpRotError,
                    ))
                }
                MsgType::ErrorRsp => Err(idol_runtime::RequestError::Runtime(
                    UpdateError::SpRotError,
                )),
                _ => {
                    ringbuf_entry!(Trace::WrongMsgType(msgtype));
                    Err(idol_runtime::RequestError::Runtime(
                        UpdateError::SpRotError,
                    ))
                }
            }
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
    ) -> Result<Received, RequestError<MsgError>> {
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
    ) -> Result<Received, RequestError<MsgError>> {
        // get available payload buffer
        if let Some(buf) =
            payload_buf_mut(None, &mut self.tx_buf[..]).get_mut(..source.len())
        {
            // self.tx.init(msgtype);
            // Read the message into our local buffer offset by the header size
            match source.read_range(0..source.len(), buf) {
                Ok(()) => {}
                Err(()) => {
                    return Err(idol_runtime::RequestError::Fail(
                        ClientError::WentAway,
                    ));
                }
            }
        } else {
            return Err(idol_runtime::RequestError::Runtime(
                MsgError::Oversize,
            ));
        }
        let size = match compose(msgtype, source.len(), &mut self.tx_buf[..]) {
            Ok(size) => size,
            Err(_err) => {
                return Err(idol_runtime::RequestError::Runtime(
                    MsgError::Serialization,
                ));
            }
        };
        // Send message, then receive response using the same local buffer.
        match self.do_send_recv_retries(size, TIMEOUT_MAX, attempts) {
            Ok((msgtype, payload_size)) => {
                let payload = payload_buf(Some(payload_size), &self.rx_buf[..]);
                if !payload.is_empty() {
                    sink.write_range(0..payload_size, payload).map_err(
                        |_| RequestError::Fail(ClientError::WentAway),
                    )?;
                }
                Ok(Received {
                    length: payload_size as u16,
                    msgtype: msgtype as u8,
                }) // XXX 'as' truncates
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
    ) -> Result<PulseStatus, RequestError<MsgError>> {
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
            // The RoT reports a errors in an ErrorRsp message.
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
            ) -> Result<SinkStatus, RequestError<MsgError>> {
                let size = size as usize;
                debug_set(&self.sys, false);

                if size > core::mem::size_of::<u16>() {
                    // The payload is big enough to contain the sequence number
                    // and additional bytes.
                    let mut n: u8 = HEADER_SIZE as u8;
                    let buf = payload_buf_mut(Some(size), &mut self.tx_buf[..]);
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
                        let seq_buf = payload_buf_mut(Some(core::mem::size_of::<u16>()), &mut self.tx_buf[..]);
                        LittleEndian::write_u16(seq_buf, sent);
                    }
                    match compose(MsgType::SinkReq, size, &mut self.tx_buf[..]) {
                        Err(_err) => break Err(MsgError::Serialization),
                        Ok(size) => {
                            match self.do_send_recv_retries(size, TIMEOUT_QUICK, MAX_SINKREQ_ATTEMPTS) {
                                Err(err) => {
                                    ringbuf_entry!(Trace::SinkFail(err, sent));
                                    break Err(err)
                                },
                                Ok((msgtype, payload_len)) => {
                                    match msgtype {
                                        MsgType::SinkRsp => {
                                            // TODO: Check sequence number in response.
                                            if payload_len >= core::mem::size_of::<u16>() {
                                                let seq_buf = payload_buf(Some(core::mem::size_of::<u16>()), &self.rx_buf[..]);
                                                let r_seqno = LittleEndian::read_u16(seq_buf);
                                                if sent != r_seqno {
                                                    break Err(MsgError::Sequence);
                                                }
                                            }
                                        },
                                        MsgType::ErrorRsp if (payload_len > 0) => {
                                            let bytes = payload_buf(Some(payload_len), &self.rx_buf[..]);
                                            if let Some(code) = MsgError::from_u8(bytes[0]) {
                                                break Err(code);
                                            } else {
                                                break Err(MsgError::Unknown);
                                            }
                                        },
                                        MsgType::ErrorRsp => {
                                            break Err(MsgError::Unknown);
                                        },
                                        _ => {
                                            // Other non-SinkRsp messages from the RoT
                                            // are not recoverable with a retry.
                                            break Err(MsgError::BadMessageType);
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
            ) -> Result<SinkStatus, RequestError<MsgError>> {
                Err(RequestError::Runtime(MsgError::NotImplemented))
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
    ) -> Result<Status, RequestError<MsgError>> {
        self.do_status().map_err(|e| e.into())
    }

    fn block_size(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<UpdateError>> {
        match self.upd(
            MsgType::UpdBlockSizeReq,
            0,
            MsgType::UpdBlockSizeRsp,
            TIMEOUT_QUICK,
            1,
        ) {
            Ok(Some(block_size)) => {
                let bs = block_size as usize;
                ringbuf_entry!(Trace::BlockSize(bs));
                // Ok(block_size as usize)
                Ok(bs)
            }
            Ok(None) => Err(idol_runtime::RequestError::Runtime(
                UpdateError::SpRotError,
            )),
            Err(e) => Err(e),
        }
    }

    fn prep_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
        image_type: UpdateTarget,
    ) -> Result<(), idol_runtime::RequestError<UpdateError>> {
        let payload = payload_buf_mut(None, &mut self.tx_buf[..]);
        let payload_len = hubpack::serialize(&mut payload[0..], &image_type)
            .map_err(|_| {
                idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
            })?;
        match self.upd(
            MsgType::UpdPrepImageUpdateReq,
            payload_len,
            MsgType::UpdPrepImageUpdateRsp,
            TIMEOUT_QUICK,
            1,
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
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
    ) -> Result<(), idol_runtime::RequestError<UpdateError>> {
        let payload = payload_buf_mut(None, &mut self.tx_buf[..]);
        let n = hubpack::serialize(&mut payload[0..], &block_num).map_err(
            |_| idol_runtime::RequestError::Runtime(UpdateError::SpRotError),
        )?;
        block
            .read_range(0..block.len(), &mut payload[n..n + block.len()])
            .map_err(|_| {
                idol_runtime::RequestError::Runtime(UpdateError::BadLength)
            })?;
        let payload_len = n + block.len();
        match self.upd(
            MsgType::UpdWriteOneBlockReq,
            payload_len,
            MsgType::UpdWriteOneBlockRsp,
            TIMEOUT_WRITE_ONE_BLOCK,
            MAX_UPD_ATTEMPTS,
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn finish_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<UpdateError>> {
        match self.upd(
            MsgType::UpdFinishImageUpdateReq,
            0,
            MsgType::UpdFinishImageUpdateRsp,
            TIMEOUT_QUICK,
            1,
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn current_version(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ImageVersion, idol_runtime::RequestError<UpdateError>> {
        let size =
            compose(MsgType::UpdCurrentVersionReq, 0, &mut self.tx_buf[..])
                .map_err(|_| {
                    idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
                })?;
        let (msgtype, payload_len) = self
            .do_send_recv_retries(size, TIMEOUT_QUICK, 2)
            .map_err(|_| {
                idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
            })?;
        if msgtype == MsgType::UpdCurrentVersionRsp {
            let buf = payload_buf(Some(payload_len), &self.rx_buf[..]);
            let (rsp, _) =
                hubpack::deserialize::<ImageVersion>(buf).map_err(|_| {
                    idol_runtime::RequestError::Runtime(UpdateError::SpRotError)
                })?;
            Ok(rsp)
        } else {
            Err(idol_runtime::RequestError::Runtime(UpdateError::SpRotError))
        }
    }

    fn abort_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<UpdateError>> {
        match self.upd(
            MsgType::UpdAbortUpdateReq,
            0,
            MsgType::UpdAbortUpdateRsp,
            TIMEOUT_QUICK,
            1,
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

mod idl {
    use super::{
        ImageVersion, MsgError, MsgType, PulseStatus, Received, SinkStatus,
        Status, UpdateError, UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
