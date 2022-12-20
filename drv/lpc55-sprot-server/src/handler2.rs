// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use drv_sprot_api::{IoStats, RxMsg2, TxMsg2, VerifiedTxMsg2};
use drv_update_api::{update_server, Update};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use sprockets_rot::RotSprocket;

mod sprockets;

task_slot!(UPDATE_SERVER, update_server);

pub struct Handler {
    sprocket: sprockets_rot::RotSprocket,
    pub update: Update,
    count: usize,
    prev: PrevMsg,
}

pub(crate) fn new() -> Handler {
    Handler {
        sprocket: crate::handler::sprockets::init(),
        update: Update::from(UPDATE_SERVER.get_task_id()),
    }
}
impl Handler {
    /// Serialize and return a `SprotError::FlowError`
    pub fn flow_error<'a>(
        &self,
        tx_buf: TxMsg2<'a>,
        stats: &mut IoStats,
    ) -> VerifiedTxMsg2<'a> {
        tx_buf.error_rsp(SprotError::FlowError);
    }

    /// Serialize and return a `SprotError::FlowError`
    pub fn protocol_error<'a>(
        &self,
        tx_buf: TxMsg2<'a>,
        stats: &mut IoStats,
    ) -> VerifiedTxMsg2<'a> {
        tx_buf.error_rsp(SprotError::ProtocolInvariantViolated);
    }

    pub fn handle<'a>(
        &mut self,
        rx_buf: RxMsg2,
        mut tx_buf: TxMsg2<'a>,
        stats: &mut IoStats,
    ) -> Option<VerifiedTxMsg2<'a>> {
        // Parse the header and validate the CRC
        let rx_msg = match rx_buf.parse() {
            Ok(rxmsg) => rxmsg,
            Err((header_bytes, msgerr)) => {
                if msgerr == SprotError::NoMessage {
                    ringbuf_entry!(Trace::IgnoreOnParse);
                    return None;
                }
                ringbuf_entry!(Trace::ErrHeader(header_bytes));
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                return Some(tx_buf.error_rsp(msgerr));
            }
        };

        // The CRC validated header and range checked length of the receiver can
        // be trusted now.
        let rx_payload = rxmsg.payload;
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
            Ok(verified_tx_msg) => {
                stats.rx_received = stats.rx_received.wrapping_add(1);
                verified_tx_msg
            }
            Err((tx_buf, err)) => {
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                ringbuf_entry!(Trace::ErrWithTypedHeader(err, rxmsg.header));
                tx_buf.error_rsp(err)
            }
        }
    }
}
