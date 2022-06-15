// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod sprockets;

use crate::IoStatus;
use drv_sprot_api::*;
// Chained if let statements are almost here.
use if_chain::if_chain;
use zerocopy::{AsBytes, LayoutVerified};

/// The driver indicates to the handler the result of the previous IO.

pub struct Handler {
    sprocket: sprockets_rot::RotSprocket,
}

pub fn new() -> Handler {
    Handler {
        sprocket: crate::handler::sprockets::init(),
    }
}

impl Handler {
    /// The Sp RoT target message handler processes the incomming message
    /// and returns the length of the response placed in the tx buffer.
    /// If the length of the tx buffer is greater than zero, the driver
    /// will interrupt the SP to notify it of the response.
    /// The driver will pad the unused portion of the Tx buffer with
    /// zeros to satisfy IO needs when the SP clocks out more bytes than
    /// available.
    ///
    /// Returns the number of bytes to transmit out of the tx buffer.
    pub fn handle(
        &mut self,
        tx_prev: bool,
        result: IoStatus,
        rx: &[u8; REQ_BUF_SIZE],
        tx: &mut [u8; RSP_BUF_SIZE],
        rlen: usize,
        status: &mut Status, // for responses and updating
    ) -> Option<usize> {
        let rmsg = if let Some(msg) = LayoutVerified::<_, Msg>::new(&rx[..]) {
            msg.into_ref()
        } else {
            status.handler_error = status.handler_error.wrapping_add(1);
            return None;
        };
        let tmsg: &mut drv_sprot_api::Msg = if let Some(msg) =
            LayoutVerified::<_, Msg>::new(tx.as_bytes_mut())
        {
            msg.into_mut()
        } else {
            status.handler_error = status.handler_error.wrapping_add(1);
            return None;
        };
        // Make sure that the number of bytes exchanged
        // do not exceed the buffer size.
        // Note that extra bytes were discarded or generated from a constant,
        // not R/W to buffers.
        let rlen = match rx.get(0..rlen) {
            Some(slice) => slice.len(),
            None => {
                if let Ok(size) = tmsg.enqueue(
                    MsgType::ErrorRsp,
                    EnqueueBuf::Copy(&[MsgError::BadMessageLength as u8]),
                ) {
                    return Some(size);
                } else {
                    status.handler_error = status.handler_error.wrapping_add(1);
                    return None;
                }
            }
        };

        // Keep count of flow errors.
        // Reject received messages if we had an overrun.
        match result {
            IoStatus::IOResult { overrun, underrun } => {
                if tx_prev && underrun {
                    status.tx_underrun = status.tx_underrun.wrapping_add(1);
                    // If the flow error was in the message as opposed to
                    // the trailing bytes, then the SP will see the CRC
                    // error and can try again.
                    // We still have the data, but if there is an Rx
                    // message to handle, ignoring that and resending our
                    // old response would be the wrong thing to do.
                }
                // In all known cases, the first bytes in the FIFO will be received
                // correctly. That includes the protocol and if it is not something we
                // ignore, then send an error message.
                if overrun && rlen > 0 && !rmsg.is_ignored_version() {
                    status.rx_overrun = status.rx_overrun.wrapping_add(1);
                    if let Ok(size) = tmsg.enqueue(
                        MsgType::ErrorRsp,
                        EnqueueBuf::Copy(&[MsgError::FlowError as u8]),
                    ) {
                        return Some(size);
                    } else {
                        panic!("A message with a one byte payload cannot fail");
                    }
                }
            }
            IoStatus::Flush => {
                if tx_prev {
                    status.tx_incomplete = status.tx_incomplete.wrapping_add(1);
                    // Our message was not delivered
                }
                return None;
            }
        }

        // The "Flush" state should always coincide with a zero length rx buffer.
        // So, if not Flush, then rlen must always be > 0 and there will be
        // a protocol identifier byte.
        // Get number of bytes to transmit or MsgError to send

        let r: Result<usize, MsgError> = if rlen < 1
            || rmsg.is_ignored_version()
        {
            // No accounting is need for this normal case
            //   - RoT sends a message, SP sends nulls
            //   - SP starts and stops a frame and clocks no data.
            Ok(0)
        } else if !rmsg.is_supported_version() {
            // Unsupported message protocol
            status.rx_invalid = status.rx_invalid.wrapping_add(1);
            Err(MsgError::UnsupportedProtocol)
        } else if rlen < (HEADER_SIZE + CRC_SIZE)
            || rmsg.unvalidated_payload_len() as usize
                > rlen - (HEADER_SIZE + CRC_SIZE)
        {
            // Short or long message.
            // Message length needs to be known and sanity checked before
            // the CRC can be calculated.
            // Note that the SP can clock out more bytes than it sent leaving
            // extra data in the receive buffer. That's ok.
            status.rx_invalid = status.rx_invalid.wrapping_add(1);
            Err(MsgError::BadMessageLength)
        } else if !rmsg.is_crc_valid() {
            // Bad CRC
            status.rx_invalid = status.rx_invalid.wrapping_add(1);
            Err(MsgError::InvalidCrc)
        } else {
            // A message arrived intact
            status.rx_received = status.rx_received.wrapping_add(1);
            // The CRC validate header and range checked length can be trusted now.
            let rlen = rmsg.unvalidated_payload_len();
            match rmsg.msgtype() {
                MsgType::EchoReq => {
                    if rlen == 0 {
                        // Empty payload
                        tmsg.enqueue(MsgType::EchoRsp, EnqueueBuf::Empty)
                    } else {
                        // Non-empty payload
                        if let Some(buf) = rmsg.payload_buf() {
                            tmsg.enqueue(
                                MsgType::EchoRsp,
                                EnqueueBuf::Copy(buf),
                            )
                        } else {
                            Err(MsgError::Unknown) // this cannot happen
                        }
                    }
                }
                MsgType::StatusReq => tmsg.enqueue(
                    MsgType::StatusRsp,
                    EnqueueBuf::Copy(status.as_bytes()),
                ),
                MsgType::SprocketsReq => {
                    if let Some(rbuf) = rmsg.payload_buf() {
                        let size = match self
                            .sprocket
                            .handle(rbuf, tmsg.payload_buf_mut())
                        {
                            Ok(size) => size,
                            Err(_) => {
                                crate::handler::sprockets::bad_encoding_rsp(
                                    tmsg.payload_buf_mut(),
                                )
                            }
                        };
                        tmsg.enqueue(
                            MsgType::SprocketsRsp,
                            EnqueueBuf::TxBuf(size),
                        )
                    } else {
                        Err(MsgError::BadMessageLength)
                    }
                }
                MsgType::SinkReq => {
                    // The first two bytes of a SinkReq payload are the U16
                    // mod 2^16 sequence number.
                    if_chain! {
                        if let Some(buf) = rmsg.payload_buf();
                        if let Some(sink_num) = buf.get(0..2);
                        then {
                            tmsg.enqueue(
                                MsgType::SinkRsp,
                                EnqueueBuf::Copy(sink_num),
                            )
                        } else {
                            tmsg.enqueue(MsgType::SinkRsp, EnqueueBuf::Empty)
                        }
                    }
                }
                // All of the unexpected messages
                MsgType::Invalid
                | MsgType::ErrorRsp
                | MsgType::EchoRsp
                | MsgType::StatusRsp
                | MsgType::SinkRsp
                | MsgType::SprocketsRsp
                | MsgType::Unknown => Err(MsgError::BadMessageType),
            }
        };
        // The above cases either enqueued a message and returned size
        // or generated 1-byte error code.
        match r {
            Ok(size) => {
                if size > 0 {
                    Some(size)
                } else {
                    None
                }
            }
            Err(err) => {
                status.rx_invalid = status.rx_invalid.wrapping_add(1);
                if let Ok(size) = tmsg
                    .enqueue(MsgType::ErrorRsp, EnqueueBuf::Copy(&[err as u8]))
                {
                    Some(size)
                } else {
                    status.handler_error = status.handler_error.wrapping_add(1);
                    None
                }
            }
        }
    }
}
