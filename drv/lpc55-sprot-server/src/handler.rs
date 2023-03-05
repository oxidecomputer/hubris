// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crc::{Crc, CRC_32_CKSUM};
use drv_sprot_api::{
    MsgType, Protocol, RotIoStats, RxMsg, SprotError, SprotStatus, TxMsg,
    UpdateRspHeader, VerifiedTxMsg, BUF_SIZE,
};
use drv_update_api::{Update, UpdateStatus, UpdateTarget};
use dumper_api::Dumper;
use lpc55_romapi::bootrom;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use sprockets_rot::RotSprocket;
use static_assertions::const_assert;
use userlib::{task_slot, UnwrapLite};

mod sprockets;

task_slot!(UPDATE_SERVER, update_server);
task_slot!(DUMPER, dumper);

pub const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_CKSUM);
const_assert!(Protocol::V1 as u8 > 0 && (Protocol::V1 as u8) < 32);

/// State that is set once at the start of the driver
pub struct StartupState {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,
    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    /// TODO: This should live in the stage0 handoff info
    pub bootrom_crc32: u32,

    /// Maxiumum message size that the RoT can handle.
    pub buffer_size: u32,
}

pub struct Handler {
    sprocket: RotSprocket,
    update: Update,
    startup_state: StartupState,
}

impl Handler {
    pub fn new() -> Handler {
        Handler {
            sprocket: crate::handler::sprockets::init(),
            update: Update::from(UPDATE_SERVER.get_task_id()),
            startup_state: StartupState {
                supported: 1_u32 << (Protocol::V1 as u8 - 1),
                bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
                buffer_size: BUF_SIZE as u32,
            },
        }
    }

    /// Serialize and return a `SprotError::FlowError`
    pub fn flow_error<'a>(&self, tx_buf: TxMsg<'a>) -> VerifiedTxMsg<'a> {
        tx_buf.error_rsp(SprotError::FlowError)
    }

    pub fn handle<'a>(
        &mut self,
        rx_buf: RxMsg,
        mut tx_buf: TxMsg<'a>,
        stats: &mut RotIoStats,
    ) -> Option<VerifiedTxMsg<'a>> {
        // Parse the header and validate the CRC
        let rx_msg = match rx_buf.parse() {
            Ok(rxmsg) => rxmsg,
            Err((header_bytes, msgerr)) => {
                if msgerr == SprotError::NoMessage {
                    // We were just returning a reply, so clocked out zeros
                    // from the SP.
                    return None;
                }
                ringbuf_entry!(Trace::ErrWithHeader(msgerr, header_bytes));
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                return Some(tx_buf.error_rsp(msgerr));
            }
        };

        // The CRC validated header and range checked length of the receiver can
        // be trusted now.
        let rx_payload = rx_msg.payload();
        let res = match rx_msg.header().msgtype {
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
                        supported: self.startup_state.supported,
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        buffer_size: self.startup_state.buffer_size,
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
                tx_buf.serialize(MsgType::IoStatsRsp, *stats)
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
            MsgType::UpdResetComponentReq => {
                match hubpack::deserialize::<drv_sprot_api::ResetComponentHeader>(
                    rx_payload,
                ) {
                    Ok((header, auth_data)) => {
                        let intent = header.intent;
                        let target = header.target;
                        let rsp: UpdateRspHeader = self
                            .update
                            .reset_component(
                                intent,
                                target,
                                auth_data.len() as u16,
                                &auth_data[..],
                            )
                            .map(|_| None)
                            .map_err(|e| e.into());
                        // TODO: Some sort of error if we didn't reset.

                        tx_buf.serialize(MsgType::UpdResetComponentRsp, rsp)
                    }
                    Err(e) => Err((tx_buf, e.into())),
                }
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
            MsgType::DumpReq => {
                let addr =
                    u32::from_le_bytes(rx_payload[0..4].try_into().unwrap());
                ringbuf_entry!(Trace::Dump(addr));

                let dumper = Dumper::from(DUMPER.get_task_id());

                let rval: u32 = if let Err(e) = dumper.dump(addr) {
                    e.into()
                } else {
                    0
                };

                let tx_payload = tx_buf.payload_mut();
                tx_payload[0..4].copy_from_slice(&rval.to_le_bytes());
                tx_buf.from_existing(MsgType::DumpRsp, 4)
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
            | MsgType::UpdResetComponentRsp
            | MsgType::IoStatsRsp
            | MsgType::DumpRsp
            | MsgType::Unknown => {
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                return Some(tx_buf.error_rsp(SprotError::BadMessageType));
            }
        };

        match res {
            Ok(verified_tx_msg) => {
                stats.rx_received = stats.rx_received.wrapping_add(1);
                Some(verified_tx_msg)
            }
            Err((tx_buf, err)) => {
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                ringbuf_entry!(Trace::ErrWithTypedHeader(err, rx_msg.header()));
                Some(tx_buf.error_rsp(err))
            }
        }
    }
}
