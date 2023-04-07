// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crc::{Crc, CRC_32_CKSUM};
use drv_sprot_api::{
    MsgType, Protocol, ReqBody, RotIoStats, RspBody, RxMsg, SprotError,
    SprotProtocolError, SprotStatus, TxMsg, UpdateReq, UpdateRsp,
    VerifiedTxMsg, BUF_SIZE,
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
    pub fn flow_error(&self, tx_buf: &mut [u8]) -> usize {
        let body = Err(SprotError::Protocol(SprotProtocolError::FlowError));
        Response::pack(body, tx_buf)
    }

    pub fn handle(
        &mut self,
        rx_buf: &[u8],
        tx_buf: &mut [u8],
        stats: &mut RotIoStats,
    ) -> usize {
        stats.rx_received = stats.rx_received.wrapping_add(1);
        let req = match Request::unpack(rx_buf) {
            Ok(req) => req,
            Err(e) => {
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                return Response::pack(Err(e.into()), tx_buf);
            }
        };

        let rsp_body = self.handle_req_body(req.body, stats);
        Response::pack(rsp_body, tx_buf)
    }

    pub fn handle_req_body(
        &mut self,
        req: ReqBody,
        stats: &mut RotIoStats,
    ) -> Result<Response, SprotError> {
        match req.body {
            ReqBody::Status => match self.update.status() {
                UpdateStatus::Rot(rot_updates) => {
                    let msg = SprotStatus {
                        supported: self.startup_state.supported,
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        buffer_size: self.startup_state.buffer_size,
                        rot_updates,
                    };
                    Ok(RspBody::Status(msg))
                }
                _ => {
                    stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                    Err(SprotError::Protocol(
                        SprotProtocolError::BadUpdateStatus,
                    ))
                }
            },
            ReqBody::IoStats => Ok(RspBody::IoStats(stats.clone())),
            ReqBody::Sprockets(req) => {
                // TODO: Don't unwrap!
                Ok(RspBody::Sprockets(
                    self.sprocket.handle_deserialized(req).unwrap_lite(),
                ))
            }
            ReqBody::Dump { addr } => {
                ringbuf_entry!(Trace::Dump(addr));
                let dumper = Dumper::from(DUMPER.get_task_id());
                match dumper.dump(addr) {
                    Ok(()) => Ok(RspBody::Ok),
                    Err(e) => SprotError::Dump(e),
                }
            }
            ReqBody::Update(UpdateReq::GetBlockSize) => {
                let size = self.update.block_size()?;
                Ok(RspBody::Update(UpdateRsp::BlockSize(size)))
            }
            ReqBody::Update(UpdateReq::Prep(target)) => {
                self.update.prep_image_update(target)?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::WriteBlock { block_num, block }) => {
                self.update.write_one_block(block_num, &block)?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::Abort) => {
                self.update.abort_update()?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::Finish) => {
                self.update.finish_image_update()?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::SwitchDefaultImage {
                slot,
                duration,
            }) => {
                self.update.switch_default_image(slot, duration)?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::Reset) => {
                self.update.reset()?;
                Ok(RspBody::Ok)
            }
        }
    }
}
