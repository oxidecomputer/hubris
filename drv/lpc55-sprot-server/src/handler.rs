// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod sprockets;

use crate::{IoStatus, LocalState};
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

pub(crate) struct Handler {
    sprocket: sprockets_rot::RotSprocket,
    pub update: Update,
    count: usize,
    prev: PrevMsg,
}

pub(crate) fn new() -> Handler {
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
    ErrHeader(usize, PrevMsg, [u8; HEADER_SIZE]),
    Overrun(usize),
    ErrWithHeader(SprotError, [u8; HEADER_SIZE]),
    ErrWithTypedHeader(SprotError, MsgHeader),
    Ignore,
    HandleMsg,
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
    pub fn handle<'a>(
        &mut self,
        iostat: IoStatus,
        rx_buf: RxMsg2,
        verified_tx_msg: VerifiedTxMsg2<'a>,
        state: &mut LocalState, // for responses and updating
    ) -> Option<VerifiedTxMsg2<'a>> {
        ringbuf_entry!(Trace::HandleMsg);
        self.count = self.count.wrapping_add(1);

        // true if previous loop transmitted.
        let tx_prev = verified_tx_msg.contains_data();
        // Let's get back a zeroed out, writable buffer
        let tx_buf = verified_tx_msg.into_txmsg();

        // Before looking at the received message, check for explicit flush or
        // a receive overrun condition.
        // Reject received messages if we had an overrun.
        match iostat {
            IoStatus::IOResult { overrun, underrun } => {
                if tx_prev && underrun {
                    state.tx_underrun = state.tx_underrun.wrapping_add(1);
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
                    return match rx_buf.protocol() {
                        None => {
                            ringbuf_entry!(Trace::Prev(self.count, self.prev));
                            ringbuf_entry!(Trace::Overrun(self.count));
                            self.prev = PrevMsg::Overrun;
                            None
                        }
                        // XXX(AJS): Previously unhandled case.
                        Some(protocol) if protocol == Protocol::Ignore => {
                            ringbuf_entry!(Trace::Ignore);
                            None
                        }
                        Some(_) => {
                            state.rx_overrun = state.rx_overrun.wrapping_add(1);
                            ringbuf_entry!(Trace::Prev(self.count, self.prev));
                            self.prev = PrevMsg::Overrun;
                            ringbuf_entry!(Trace::ErrHeader(
                                self.count,
                                self.prev,
                                rx_buf.header_bytes()
                            ));
                            return Some(
                                tx_buf.error_rsp(SprotError::FlowError),
                            );
                        }
                    };
                }
            }
            IoStatus::Flush => {
                if tx_prev {
                    state.tx_incomplete = state.tx_incomplete.wrapping_add(1);
                    // Our message was not delivered
                }
                self.prev = PrevMsg::Flush;
                return None;
            }
        }

        // Check for the minimum receive length being satisfied.
        if rx_buf.len() < MIN_MSG_SIZE {
            let err = SprotError::BadMessageLength;
            ringbuf_entry!(Trace::ErrWithHeader(err, rx_buf.header_bytes()));
            return Some(tx_buf.error_rsp(err));
        }

        // Parse the header and validate the CRC
        let rxmsg = match rx_buf.parse() {
            Ok(rxmsg) => {
                self.prev = PrevMsg::Good(rxmsg.header.msgtype);
                rxmsg
            }
            Err((header_bytes, msgerr)) => {
                if msgerr == SprotError::NoMessage {
                    self.prev = PrevMsg::None;
                    return None;
                }
                ringbuf_entry!(Trace::ErrHeader(
                    self.count,
                    self.prev,
                    header_bytes
                ));
                return Some(tx_buf.error_rsp(msgerr));
            }
        };

        // At this point, the header and payload are known to be
        // consistent with the CRC and the length is known to be good.
        state.rx_received = state.rx_received.wrapping_add(1);

        Some(self.run(rxmsg, tx_buf, state))
    }

    // Run the command for the given MsgType, serialize the reply into `tx_output` and return the response MsgType
    // and payload size or return an error.
    fn run<'a>(
        &mut self,
        rxmsg: VerifiedRxMsg2,
        mut tx_buf: TxMsg2<'a>,
        state: &mut LocalState,
    ) -> VerifiedTxMsg2<'a> {
        let rx_payload = rxmsg.payload;
        // The CRC validate header and range checked length of the receiver can be trusted now.
        let res = match rxmsg.header.msgtype {
            MsgType::EchoReq => {
                // We know payload_len is within bounds since the received
                // header was parsed successfully and the send and receive
                // buffers are the same size.
                let tx_payload = tx_buf.payload_mut();
                let dst = &mut tx_payload[..rx_payload.len()];
                dst.copy_from_slice(rx_payload);
                let payload_len = dst.len();
                tx_buf.from_existing(MsgType::EchoRsp, payload_len)
            }
            MsgType::StatusReq => match self.update.status() {
                UpdateStatus::Rot(rot_updates) => {
                    let msg = SprotStatus {
                        supported: state.supported,
                        bootrom_crc32: state.bootrom_crc32,
                        buffer_size: state.buffer_size,
                        rot_updates,
                    };
                    tx_buf.serialize(MsgType::StatusRsp, msg)
                }
                UpdateStatus::LoadError(_) => {
                    Err((tx_buf, SprotError::Stage0HandoffError))
                }
                UpdateStatus::Sp => Err((tx_buf, SprotError::UpdateBadStatus)),
            },
            MsgType::IoStatsReq => {
                let msg = IoStats {
                    rx_received: state.rx_received,
                    rx_overrun: state.rx_overrun,
                    tx_underrun: state.tx_underrun,
                    rx_invalid: state.rx_invalid,
                    tx_incomplete: state.tx_incomplete,
                };
                tx_buf.serialize(MsgType::IoStatsRsp, msg)
            }
            MsgType::SprocketsReq => {
                let tx_payload = tx_buf.payload_mut();
                let n = self
                    .sprocket
                    .handle(rx_payload, tx_payload)
                    .unwrap_or_else(|_| {
                        crate::handler::sprockets::bad_encoding_rsp(tx_payload)
                    });
                tx_buf.from_existing(MsgType::SprocketsRsp, n)
            }
            MsgType::UpdBlockSizeReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .block_size()
                    .map(|size| Some(size.try_into().unwrap_lite()))
                    .map_err(|err| err.into());
                tx_buf.serialize(MsgType::UpdBlockSizeRsp, rsp)
            }
            MsgType::UpdPrepImageUpdateReq => {
                match hubpack::deserialize::<UpdateTarget>(rx_payload) {
                    Ok((image_type, _n)) => {
                        let rsp: UpdateRspHeader = self
                            .update
                            .prep_image_update(image_type)
                            .map(|_| None)
                            .map_err(|e| e.into());
                        tx_buf.serialize(MsgType::UpdPrepImageUpdateRsp, rsp)
                    }
                    Err(e) => Err((tx_buf, e.into())),
                }
            }
            MsgType::UpdWriteOneBlockReq => {
                match hubpack::deserialize::<u32>(rx_payload) {
                    Ok((block_num, block)) => {
                        let rsp: UpdateRspHeader = self
                            .update
                            .write_one_block(block_num as usize, block)
                            .map(|_| None)
                            .map_err(|e| e.into());

                        tx_buf.serialize(MsgType::UpdWriteOneBlockRsp, rsp)
                    }
                    Err(e) => Err((tx_buf, e.into())),
                }
            }

            MsgType::UpdAbortUpdateReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .abort_update()
                    .map(|_| None)
                    .map_err(|e| e.into());
                tx_buf.serialize(MsgType::UpdAbortUpdateRsp, rsp)
            }
            MsgType::UpdFinishImageUpdateReq => {
                let rsp: UpdateRspHeader = self
                    .update
                    .finish_image_update()
                    .map(|_| None)
                    .map_err(|e| e.into());
                tx_buf.serialize(MsgType::UpdFinishImageUpdateRsp, rsp)
            }
            MsgType::SinkReq => {
                // The first two bytes of a SinkReq payload are the U16
                // mod 2^16 sequence number.
                if rx_payload.len() >= 2 {
                    let tx_payload = tx_buf.payload_mut();
                    tx_payload[..2].copy_from_slice(&rx_payload[..2]);
                    tx_buf.from_existing(MsgType::SinkRsp, 2)
                } else {
                    Ok(tx_buf.no_payload(MsgType::SinkRsp))
                }
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
            | MsgType::IoStatsRsp
            | MsgType::Unknown => {
                state.rx_invalid = state.rx_invalid.wrapping_add(1);
                return tx_buf.error_rsp(SprotError::BadMessageType);
            }
        };

        match res {
            Ok(verified_tx_msg) => verified_tx_msg,
            Err((tx_buf, err)) => {
                ringbuf_entry!(Trace::ErrWithTypedHeader(err, rxmsg.header));
                tx_buf.error_rsp(err)
            }
        }
    }
}
