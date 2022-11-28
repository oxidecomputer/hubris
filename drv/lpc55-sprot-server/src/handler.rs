// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod sprockets;

use crate::IoStatus;
use drv_sprot_api::*;
use drv_update_api::*;
use ringbuf::*;
use userlib::*;

task_slot!(UPDATE_SERVER, update_server);

#[derive(Copy, Clone, PartialEq)]
enum PrevMsg {
    None,
    Flush,
    Good(MsgType),
    Overrun,
}

pub struct Handler {
    sprocket: sprockets_rot::RotSprocket,
    pub update: Update,
    count: usize,
    prev: PrevMsg,
}

pub fn new() -> Handler {
    Handler {
        sprocket: crate::handler::sprockets::init(),
        update: drv_update_api::Update::from(UPDATE_SERVER.get_task_id()),
        prev: PrevMsg::None,
        count: 0,
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Prev(usize, PrevMsg),
    ErrHeader(usize, PrevMsg, u8, u8, u8, u8),
    Overrun(usize),
}
ringbuf!(Trace, 16, Trace::None);

impl Handler {
    /// The Sp RoT target message handler processes the incoming message
    /// and returns the length of the response placed in the Tx buffer.
    /// If the length of the Tx buffer is greater than zero, the driver
    /// will interrupt the SP to notify it of the response.
    /// The driver will pad the unused portion of the Tx buffer with
    /// zeros to satisfy IO needs when the SP clocks out more bytes than
    /// available.
    ///
    /// Returns the number of bytes to transmit out of the Tx buffer.
    pub fn handle(
        &mut self,
        tx_prev: bool,
        iostat: IoStatus,
        rx_buf: &[u8],
        tx_buf: &mut TxMsg,
        status: &mut Status, // for responses and updating
    ) -> Option<usize> {
        let tx_payload = tx_buf.payload_mut();
        self.count = self.count.wrapping_add(1);

        // Before looking at the received message, check for explicit flush or
        // a receive overrun condition.
        // Reject received messages if we had an overrun.
        match iostat {
            IoStatus::IOResult { overrun, underrun } => {
                if tx_prev && underrun {
                    status.tx_underrun = status.tx_underrun.wrapping_add(1);
                    // If the flow error was in the message as opposed to
                    // possible post-message trailing bytes, then the SP will
                    // see the CRC error and can try again.
                    // We discard our own possibly-failed Tx data and the SP
                    // can retry if it wants to.
                }
                // In all known cases, the first ${FIFO_LENGTH}-bytes in the
                // FIFO will be received correctly.
                // That includes the protocol identifier.
                // If it is not an ignored protocol, then send an error.
                if overrun {
                    if !rx_buf.is_empty() {
                        if Protocol::from(rx_buf[0]) != Protocol::Ignore {
                            status.rx_overrun =
                                status.rx_overrun.wrapping_add(1);
                            ringbuf_entry!(Trace::Prev(self.count, self.prev));
                            self.prev = PrevMsg::Overrun;
                            ringbuf_entry!(Trace::ErrHeader(
                                self.count, self.prev, rx_buf[0], rx_buf[1],
                                rx_buf[2], rx_buf[3]
                            ));
                            return Some(
                                tx_buf.error_rsp(SprotError::FlowError),
                            );
                        }
                    } else {
                        ringbuf_entry!(Trace::Prev(self.count, self.prev));
                        ringbuf_entry!(Trace::Overrun(self.count));
                        self.prev = PrevMsg::Overrun;
                        return None;
                    }
                }
            }
            IoStatus::Flush => {
                if tx_prev {
                    status.tx_incomplete = status.tx_incomplete.wrapping_add(1);
                    // Our message was not delivered
                }
                self.prev = PrevMsg::Flush;
                return None;
            }
        }

        // Check for the minimum receive length being satisfied.
        if rx_buf.len() < MIN_MSG_SIZE {
            return Some(tx_buf.error_rsp(SprotError::BadMessageLength));
        }

        // Parse the header which also checks the CRC.
        let (msgtype, rx_payload) = match parse(rx_buf) {
            Ok((msgtype, payload)) => {
                self.prev = PrevMsg::Good(msgtype);
                (msgtype, payload)
            }
            Err(msgerr) => {
                if msgerr == SprotError::NoMessage {
                    self.prev = PrevMsg::None;
                    return None;
                }
                ringbuf_entry!(Trace::ErrHeader(
                    self.count, self.prev, rx_buf[0], rx_buf[1], rx_buf[2],
                    rx_buf[3]
                ));
                return Some(tx_buf.error_rsp(msgerr));
            }
        };

        // At this point, the header and payload are known to be
        // consistent with the CRC and the length is known to be good.
        status.rx_received = status.rx_received.wrapping_add(1);

        // The above cases either enqueued a message and returned size
        // or generated 1-byte error code.
        match self.run(msgtype, rx_payload, tx_payload, status) {
            Ok((msgtype, payload_size)) => {
                tx_buf.from_existing(msgtype, payload_size).ok()
            }
            Err(err) if err == SprotError::NoMessage => None,
            Err(err) => Some(tx_buf.error_rsp(err)),
        }
    }

    // Run the command for the given MsgType, serialize the reply into `tx_output` and return the response MsgType
    // and payload size or return an error.
    fn run(
        &mut self,
        msgtype: MsgType,
        rx_payload: &[u8],
        tx_payload: &mut [u8],
        status: &mut Status,
    ) -> Result<(MsgType, usize), SprotError> {
        // The CRC validate header and range checked length can be trusted now.
        let size = match msgtype {
            MsgType::EchoReq => {
                if rx_payload.is_empty() {
                    0
                } else if let Some(dst) =
                    tx_payload.get_mut(0..rx_payload.len())
                {
                    dst.copy_from_slice(rx_payload);
                    dst.len()
                } else {
                    return Err(SprotError::BadMessageLength);
                }
            }
            MsgType::StatusReq => hubpack::serialize(tx_payload, &status)?,
            MsgType::SprocketsReq => {
                self.sprocket.handle(rx_payload, tx_payload).unwrap_or_else(
                    |_| crate::handler::sprockets::bad_encoding_rsp(tx_payload),
                )
            }
            MsgType::UpdBlockSizeReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .block_size()
                    .map(|size| Some(size.try_into().unwrap_lite()))
                    .map_err(|err| err.into());
                hubpack::serialize(tx_payload, &rsp)?
            }
            MsgType::UpdPrepImageUpdateReq => {
                let (image_type, _n) =
                    hubpack::deserialize::<UpdateTarget>(rx_payload)?;
                let rsp: UpdateRspHeader = self
                    .update
                    .prep_image_update(image_type)
                    .map(|_| None)
                    .map_err(|e| e.into());
                hubpack::serialize(tx_payload, &rsp)?
            }
            MsgType::UpdWriteOneBlockReq => {
                let (block_num, block) =
                    hubpack::deserialize::<u32>(rx_payload)?;
                let rsp: UpdateRspHeader = self
                    .update
                    .write_one_block(block_num as usize, block)
                    .map(|_| None)
                    .map_err(|e| e.into());
                hubpack::serialize(tx_payload, &rsp)?
            }

            MsgType::UpdAbortUpdateReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .abort_update()
                    .map(|_| None)
                    .map_err(|e| e.into());
                hubpack::serialize(tx_payload, &rsp)?
            }
            MsgType::UpdFinishImageUpdateReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .finish_image_update()
                    .map(|_| None)
                    .map_err(|e| e.into());
                hubpack::serialize(tx_payload, &rsp)?
            }
            MsgType::UpdCurrentVersionReq => {
                let rsp: Result<ImageVersion, u32> =
                    self.update.current_version().map_err(|e| e.into());
                hubpack::serialize(tx_payload, &rsp)?
            }
            MsgType::SinkReq => {
                // The first two bytes of a SinkReq payload are the U16
                // mod 2^16 sequence number.
                tx_payload[0..2].copy_from_slice(&rx_payload[0..2]);
                2
            }
            // All of the unexpected messages
            MsgType::Invalid
            | MsgType::EchoRsp
            | MsgType::ErrorRsp
            | MsgType::SinkRsp
            | MsgType::SprocketsRsp
            | MsgType::StatusRsp
            | MsgType::UpdBlockSizeRsp
            | MsgType::UpdPrepImageUpdateRsp
            | MsgType::UpdWriteOneBlockRsp
            | MsgType::UpdAbortUpdateRsp
            | MsgType::UpdFinishImageUpdateRsp
            | MsgType::UpdCurrentVersionRsp
            | MsgType::Unknown => {
                status.rx_invalid = status.rx_invalid.wrapping_add(1);
                return Err(SprotError::BadMessageType);
            }
        };

        Ok((req_msgtype_to_rsp_msgtype(msgtype), size))
    }
}

// Translate a request msg type to a response msg type
fn req_msgtype_to_rsp_msgtype(msgtype: MsgType) -> MsgType {
    match msgtype {
        MsgType::EchoReq => MsgType::EchoRsp,
        MsgType::StatusReq => MsgType::StatusRsp,
        MsgType::SprocketsReq => MsgType::SprocketsRsp,
        MsgType::UpdBlockSizeReq => MsgType::UpdBlockSizeRsp,
        MsgType::UpdPrepImageUpdateReq => MsgType::UpdPrepImageUpdateRsp,
        MsgType::UpdWriteOneBlockReq => MsgType::UpdWriteOneBlockRsp,
        MsgType::UpdAbortUpdateReq => MsgType::UpdAbortUpdateRsp,
        MsgType::UpdFinishImageUpdateReq => MsgType::UpdFinishImageUpdateRsp,
        MsgType::UpdCurrentVersionReq => MsgType::UpdCurrentVersionRsp,
        MsgType::SinkReq => MsgType::SinkRsp,

        // All of the unexpected messages
        MsgType::Invalid
        | MsgType::EchoRsp
        | MsgType::ErrorRsp
        | MsgType::SinkRsp
        | MsgType::SprocketsRsp
        | MsgType::StatusRsp
        | MsgType::UpdBlockSizeRsp
        | MsgType::UpdPrepImageUpdateRsp
        | MsgType::UpdWriteOneBlockRsp
        | MsgType::UpdAbortUpdateRsp
        | MsgType::UpdFinishImageUpdateRsp
        | MsgType::UpdCurrentVersionRsp
        | MsgType::Unknown => {
            panic!("MsgType is not a request: {}", msgtype as u8)
        }
    }
}
