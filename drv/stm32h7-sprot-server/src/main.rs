// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_spi_api::{CsState, Spi};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{ClientError, Leased, RequestError, R, W};
use zerocopy::LayoutVerified;
#[cfg(feature = "sink_test")]
use zerocopy::{ByteOrder, LittleEndian};
// Chained if let statements are almost here.
use if_chain::if_chain;
use ringbuf::*;
use userlib::*;

task_slot!(SPI, spi_driver);
task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    UnexpectedRotIrq,
    SendRecv,
    RotReadyTimeout,
    RotNotReady,
    CSnAssert,
    InvalidCRC,
    FailedRetries { retries: u16, errcode: MsgError },
}
ringbuf!(Trace, 16, Trace::None);

#[cfg(feature = "sink_test")]
#[derive(Copy, Clone, PartialEq)]
enum TraceSink {
    None,
    Count(u16),
    Size(u16),
}
#[cfg(feature = "sink_test")]
ringbuf!(SINK, TraceSink, 16, TraceSink::None);

const SP_TO_ROT_SPI_DEVICE: u8 = 0;

// TODO: These timeouts are somewhat arbitrary.

/// Timeout for status message
const TIMEOUT_QUICK: u32 = 500;
/// Maximum timeout for an arbitrary message
const TIMEOUT_MAX: u32 = 2_000;

// ROT_IRQ comes from app.toml
// We use spi3 on gimletlet and spi4 on gemini and gimlet.
// You should be able to move the RoT board between SPI3, SPI4, and SPI6
// without much trouble even though SPI3 is the preferred connector and
// SPI4 is connected to the NET board.
cfg_if::cfg_if! {
    if #[cfg(any(target_board = "gimlet-b", target_board = "gemini-bu-1"))] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            // On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ
            // on pin2/PE3
            port: sys_api::Port::E,
            pin_mask: 1 << 3,
        };
    } else if #[cfg(target_board = "gimletlet-2")] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            port: sys_api::Port::D,
            pin_mask: 1 << 0,
        };
    } else {
        compile_error!("No configuration for ROT_IRQ");
    }
}

pub struct ServerImpl {
    sys: sys_api::Sys,
    spi: drv_spi_api::SpiDevice,
    // Use separate buffers so that retries can be generic.
    pub txmsg: Msg,
    pub rxmsg: Msg,
}

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(SP_TO_ROT_SPI_DEVICE);
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::None)
        .unwrap_lite();

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        sys,
        spi,
        txmsg: Msg::new(),
        rxmsg: Msg::new(),
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

impl ServerImpl {
    /// Handle the mechanics of sending a message and waiting for a response.
    fn do_send_recv(&mut self, timeout: u32) -> Result<usize, MsgError> {
        ringbuf_entry!(Trace::SendRecv);
        // Polling and timeout configuration
        // TODO: Use EXTI interrupt and just a timeout, no polling.

        // Assume that self.txmsg contains a valid message.

        if self.is_rot_irq_asserted() {
            ringbuf_entry!(Trace::UnexpectedRotIrq);
            // TODO: Move README.md to RFD 317 and discuss:
            //   - Unsolicited messages from RoT to SP.
            //   - Ignoring message from RoT to SP.
            //   - Should we send a message telling RoT that SP has booted?
            //
            // For now, we are surprised that ROT_IRQ is asserted
            //
            // The RoT must be able to observe SP resets.
            // During the normal start-up seqeunce, the RoT is controlling the
            // SP's boot up sequence. However, the SP can reset itself and
            // individual Hubris tasks may fail and be restarted.
            // then we'll see ROT_IRQ asserted without having first sent a message.
            // If SP and RoT are out of sync, e.g. this task restarts and an old
            // response is still in the RoT's transmit FIFO, then we can also see
            // ROT_IRQ asserted when not expected.
            // TODO: configuration parameters for delays below
            if !self.wait_rot_irq(false, TIMEOUT_QUICK)
                && self.do_pulse_cs(10_u64, 10_u64)?.rot_irq_end == 1
            {
                // Did not clear ROT_IRQ
                return Err(MsgError::RotNotReady);
            }
        }
        let buf = match self.txmsg.bytes() {
            None => {
                return Err(MsgError::BadMessageLength);
            }
            Some(buf) => buf,
        };
        if self.spi.write(buf).is_err() {
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

        // all of the error returns until CS de-assert need to de-assert.
        let result = match self.spi.read(self.rxmsg.header_buf_mut()) {
            Ok(()) => {
                // Check the header before using it to fetch optional payload.
                if self.rxmsg.is_ignored_version() {
                    Err(MsgError::EmptyMessage)
                } else if self.rxmsg.is_busy_version() {
                    // This should never happen. Indicates SP/RoT out of sync.
                    Err(MsgError::RotBusy)
                } else if !self.rxmsg.is_supported_version() {
                    Err(MsgError::UnsupportedProtocol)
                } else {
                    match self.rxmsg.payload_len() {
                        Some(rlen) => {
                            if let Some(buf) = self
                                .rxmsg
                                .payload_buf_mut()
                                .get_mut(..rlen + CRC_SIZE)
                            {
                                if self.spi.read(buf).is_err() {
                                    Err(MsgError::SpiServerError)
                                } else if !self.rxmsg.is_crc_valid() {
                                    ringbuf_entry!(Trace::InvalidCRC);
                                    Err(MsgError::InvalidCrc)
                                } else {
                                    Ok(rlen)
                                }
                            } else {
                                Err(MsgError::BadTransferSize)
                            }
                        }
                        None => Err(MsgError::BadTransferSize),
                    }
                }
            }
            _ => Err(MsgError::SpiServerError),
        };

        if self.spi.release().is_err() {
            Err(MsgError::SpiServerError)
        } else {
            result
        }
    }

    fn do_send_recv_retries(
        &mut self,
        timeout: u32,
        retries: u16,
    ) -> Result<usize, MsgError> {
        let mut attempts_left = retries;
        let mut errcode = MsgError::Unknown;
        loop {
            if attempts_left == 0 {
                ringbuf_entry!(Trace::FailedRetries { retries, errcode });
                break;
            }
            attempts_left -= 1;

            let result = self.do_send_recv(timeout);
            match result {
                // All of the unrecoverable errors.
                Err(MsgError::BadMessageLength) // just shouldn't happen
                    | Err(MsgError::BadMessageType) // not sent from SP
                    | Err(MsgError::RotNotReady)    // SP can't sync RoT or T.O.
                    | Err(MsgError::RotBusy)    // SP bug?
                    | Err(MsgError::UnsupportedProtocol) // SP bug?
                    | Err(MsgError::BadTransferSize) // RoT sends bad len.
                    | Err(MsgError::BadResponse)
                    | Err(MsgError::SpiServerError) // SPI driver fail

                    // TODO: sort out the local vs remote error codes
                    | Err(MsgError::FlowError)
                    | Err(MsgError::Oversize)
                    | Err(MsgError::TxNotIdle)
                    | Err(MsgError::CannotAssertCSn)
                    | Err(MsgError::RspTimeout)
                    | Err(MsgError::NotImplemented)
                    | Err(MsgError::NonRotError)
                    | Err(MsgError::Unknown) => {
                        if let Err(err) = result {
                            errcode = err;
                        } else {
                            errcode = MsgError::Unknown;    // This cannot happen.
                        }
                        break;
                    },

                    // Recoverable errors
                Err(MsgError::InvalidCrc) => {
                    // req_crc_err = req_crc_err.wrapping_add(1);
                    errcode = MsgError::InvalidCrc;
                    continue;
                },
                Err(MsgError::EmptyMessage) => {
                    errcode = MsgError::EmptyMessage;
                    continue;
                },
                Ok(payload_len) => {
                    match self.rxmsg.msgtype() {
                        MsgType::ErrorRsp if payload_len > 0 => {
                            if_chain! {
                                if let Some(bytes) = self.rxmsg.payload_buf();
                                if let Some(bytes) = bytes.get(0..1);
                                if let Some(code) = MsgError::from_u8(bytes[0]);
                                then {
                                    match code {
                                        MsgError::FlowError => {
                                            errcode = code;
                                            // flow_err = flow_err.wrapping_add(1);
                                            continue;
                                        },
                                        MsgError::InvalidCrc => {
                                            errcode = code;
                                            // rsp_crc_err = rsp_crc_err.wrapping_add(1);
                                            continue;
                                        },
                                        _ => {
                                            // Other codes from RoT
                                            // are not recoverable
                                            // with a retry.
                                            errcode = code;
                                            break;
                                        },
                                    }
                                }
                            }
                        },
                        MsgType::ErrorRsp => {
                            errcode = MsgError::Unknown;
                            continue;
                        },
                        _ => {
                            // All of the non-error message types are ok here.
                            // XXX which length gets returned???
                            return result;
                        },
                    }
                },
            }
        }
        Err(errcode)
    }

    /// Retrieve low-level RoT status
    fn do_status(&mut self) -> Result<Status, MsgError> {
        self.txmsg.init(MsgType::StatusReq, 0);
        self.txmsg.set_crc(); // XXX enqueue(EnqueueBuf::Empty) would set the CRC.
        self.do_send_recv(TIMEOUT_QUICK)?;
        if_chain! {
            if let Some(buf) = self.rxmsg.payload_buf();
            // TODO: Do we need to allow for unligned?
            if let Some(status) = LayoutVerified::<_, Status>::new(buf);
            then {
                return Ok(*status);
            }
        }
        Err(MsgError::BadMessageLength)
    }

    /// Clear the ROT_IRQ and the RoT's Tx buffer by toggling the CSn signal.
    /// ROT_IRQ before and after state is returned for testing.
    fn do_pulse_cs(
        &mut self,
        delay: u64,
        delay_after: u64,
    ) -> Result<SpRotPulseStatus, MsgError> {
        let rot_irq_begin = self.is_rot_irq_asserted();
        self.spi
            .lock(CsState::Asserted)
            .map_err(|_| MsgError::CannotAssertCSn)?;
        if delay != 0 {
            hl::sleep_for(delay);
        }
        self.spi.release().unwrap_lite();
        if delay_after != 0 {
            hl::sleep_for(delay); // TODO: make this a 2nd parameter?
        }
        let rot_irq_end = self.is_rot_irq_asserted();
        let status = SpRotPulseStatus {
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
}

impl idl::InOrderSpRotImpl for ServerImpl {
    /// Send a message to the RoT for processing.
    fn send_recv(
        &mut self,
        recv_msg: &RecvMessage,
        msgtype: drv_sprot_api::MsgType,
        source: Leased<R, [u8]>,
        sink: Leased<W, [u8]>,
    ) -> Result<SpRotReturn, RequestError<MsgError>> {
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
    ) -> Result<SpRotReturn, RequestError<MsgError>> {
        self.txmsg.init(msgtype, source.len());
        // Read the message into our local buffer offset by the header size
        if let Some(buf) = self.txmsg.payload_buf_mut().get_mut(0..source.len())
        {
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
        self.txmsg.set_crc();

        // Send message, then receive response using the same local buffer.
        self.do_send_recv_retries(TIMEOUT_MAX, attempts)?;
        if let Some(buf) = self.rxmsg.payload_buf() {
            sink.write_range(0..buf.len(), buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        } // else empty payload is ok.
        Ok(SpRotReturn {
            msgtype: self.rxmsg.msgtype() as u8,
            length: self.rxmsg.unvalidated_payload_len() as u32,
        })
    }

    /// Clear the RoT Tx buffer and have the RoT deassert ROT_IRQ.
    /// The status of ROT_IRQ before and after the assert is returned.
    fn pulse_cs(
        &mut self,
        _: &RecvMessage,
        delay: u16,
    ) -> Result<SpRotPulseStatus, RequestError<MsgError>> {
        // If ROT_IRQ is asserted (a response is pending)
        // ROT_IRQ should be deasserted in response to CSn pulse.
        self.do_pulse_cs(delay.into(), delay.into())
            .map_err(|e| e.into())
    }

    cfg_if::cfg_if! {
        if #[cfg(feature = "sink_test")] {

            // TODO: The error handling for transport issues in rot_sink()
            // needs to be available to any client.

            /// Send `count` buffers of `size` size to simulate a firmare
            /// update or other bulk data transfer from the SP to the RoT.
            fn rot_sink(
                &mut self,
                _: &RecvMessage,
                count: u16,
                size: u16,
            ) -> Result<SpRotSinkStatus, RequestError<MsgError>> {
                let size = size as usize;
                // The writable payload_buf_mut() is the entire buffer while
                // payload_buf() is limited by the length in the header.
                if size > self.txmsg.payload_buf_mut().len() {
                    return Err(idol_runtime::RequestError::Runtime(
                            MsgError::Oversize,
                    ));
                }
                // The RoT will read all of the bytes of a MsgType::SinkReq and
                // include the sequence number in the SinkRsp.
                //
                // The RoT reports a errors in an ErrorRsp message.
                //
                // Put a known pattern into the buffer so that most of the
                // received bytes match their buffer index modulo 0x100.
                // That makes an SP or RoT underrun easier to spot on a logic
                // analyzer.
                //
                // The same buffer here is used for transmit and receive.
                // It happens that the payload portion of the buffer
                // is only slightly modified by the two possible responses.
                // The first two payload bytes are the u16 msg sequence number.
                // None of responses is expected to exceed a 2-byte payload
                // plus trailing 2-byte checksum.
                // The header and the first four bytes of the payload will be
                // updated each time through the loop.
                self.txmsg.init(MsgType::SinkReq, size);    // Tx msg minor update each iteration.
                let mut n: u8 = self.txmsg.header_buf().len() as u8;
                if let Some(buf) = self.txmsg.payload_buf_mut().get_mut(0..size) {
                    buf.fill_with(|| {
                        let seq = n;
                        n = n.wrapping_add(1);
                        seq
                    });
                }
                let mut sent = 0u16;
                const MAX_SINKREQ_ATTEMPTS: u16 = 4;
                loop {
                    if sent == count {
                        break;
                    }
                    // For debugging: first two payload bytes are a message
                    // sequence number if there is space for it.
                    if let Some(seqno) = self.txmsg.payload_buf_mut().get_mut(0..2) {
                        LittleEndian::write_u16(seqno, sent);
                    }
                    self.txmsg.set_crc();

                    let result: Result<(), MsgError> = match self.do_send_recv_retries(TIMEOUT_QUICK, MAX_SINKREQ_ATTEMPTS) {
                        Err(err) => Err(err),
                        Ok(payload_len) => {
                            match self.rxmsg.msgtype() {
                                MsgType::SinkRsp => {
                                    // TODO: Check sequence number in response.
                                    Ok(())
                                },
                                MsgType::ErrorRsp if payload_len > 0 => {
                                    if_chain! {
                                        if let Some(bytes) = self.rxmsg.payload_buf();
                                        if let Some(bytes) = bytes.get(0..1);
                                        if let Some(code) = MsgError::from_u8(bytes[0]);
                                        then {
                                            Err(code)
                                        } else {
                                            Err(MsgError::Unknown)
                                        }
                                    }
                                },
                                MsgType::ErrorRsp => {
                                    Err(MsgError::Unknown)
                                },
                                _ => {
                                    // Other codes from RoT
                                    // are not recoverable
                                    // with a retry.
                                    Err(MsgError::Unknown)
                                },
                            }
                        },
                    };
                    match result {
                        Ok(()) => {
                            sent = sent.wrapping_add(1);
                        },
                        Err(error) => {
                            return Err(RequestError::Runtime(error));
                        },
                    }
                }
                Ok(SpRotSinkStatus { sent })
            }
        } else {
            fn rot_sink(
                &mut self,
                _: &RecvMessage,
                _count: u16,
                _size: u16,
            ) -> Result<SpRotSinkStatus, RequestError<MsgError>> {
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
}

mod idl {
    use super::{
        MsgError, MsgType, SpRotPulseStatus, SpRotReturn, SpRotSinkStatus,
        Status,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
