// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crc::{Crc, CRC_32_CKSUM};
use drv_sprot_api::{
    DumpReq, DumpRsp, ReqBody, Request, Response, RotIoStats, RotState,
    RotStatus, RspBody, SprocketsError, SprotError, SprotProtocolError,
    UpdateReq, UpdateRsp, CURRENT_VERSION, MIN_VERSION, REQUEST_BUF_SIZE,
    RESPONSE_BUF_SIZE,
};
use drv_update_api::{Update, UpdateStatus};
use dumper_api::Dumper;
use lpc55_romapi::bootrom;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use sprockets_rot::RotSprocket;
use userlib::{task_slot, UnwrapLite};

mod sprockets;

task_slot!(UPDATE_SERVER, update_server);
task_slot!(DUMPER, dumper);

pub const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_CKSUM);

/// State that is set once at the start of the driver
pub struct StartupState {
    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    pub bootrom_crc32: u32,

    /// Maxiumum request size that the RoT can handle.
    pub max_request_size: u16,

    /// Maximum response size returned from the RoT to the SP
    pub max_response_size: u16,
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
                bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
                max_request_size: REQUEST_BUF_SIZE.try_into().unwrap_lite(),
                max_response_size: RESPONSE_BUF_SIZE.try_into().unwrap_lite(),
            },
        }
    }

    /// Serialize and return a `SprotError::FlowError`
    pub fn flow_error(&self, tx_buf: &mut [u8; RESPONSE_BUF_SIZE]) -> usize {
        let body = Err(SprotProtocolError::FlowError.into());
        Response::pack(&body, tx_buf)
    }

    pub fn handle(
        &mut self,
        rx_buf: &[u8],
        tx_buf: &mut [u8; RESPONSE_BUF_SIZE],
        stats: &mut RotIoStats,
    ) -> usize {
        stats.rx_received = stats.rx_received.wrapping_add(1);
        let rsp_body = match Request::unpack(rx_buf) {
            Ok(request) => self.handle_request(request, stats),
            Err(e) => {
                ringbuf_entry!(Trace::Err(e));
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                Err(e.into())
            }
        };

        Response::pack(&rsp_body, tx_buf)
    }

    pub fn handle_request(
        &mut self,
        req: Request,
        stats: &mut RotIoStats,
    ) -> Result<RspBody, SprotError> {
        match req.body {
            ReqBody::Status => {
                let status = RotStatus {
                    version: CURRENT_VERSION,
                    min_version: MIN_VERSION,
                    request_buf_size: self.startup_state.max_request_size,
                    response_buf_size: self.startup_state.max_response_size,
                };
                Ok(RspBody::Status(status))
            }
            ReqBody::IoStats => Ok(RspBody::IoStats(stats.clone())),
            ReqBody::RotState => match self.update.status() {
                UpdateStatus::Rot(state) => {
                    let msg = RotState::V1 {
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        state,
                    };
                    Ok(RspBody::RotState(msg))
                }
                _ => {
                    stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                    Err(SprotProtocolError::BadUpdateStatus)?
                }
            },
            ReqBody::Sprockets(req) => Ok(RspBody::Sprockets(
                // The only error we can get here is a serialization error,
                // which is represented as `BadEncoding`.
                self.sprocket
                    .handle_deserialized(req)
                    .map_err(|_| SprocketsError::BadEncoding)?,
            )),
            ReqBody::Dump(DumpReq::V1 { addr }) => {
                ringbuf_entry!(Trace::Dump(addr));
                let dumper = Dumper::from(DUMPER.get_task_id());
                let err = dumper.dump(addr).err();
                Ok(RspBody::Dump(DumpRsp::V1 { err }))
            }
            ReqBody::Update(UpdateReq::GetBlockSize) => {
                let size = self.update.block_size()?;
                // Block size will always fit in a u32 on these MCUs
                Ok(RspBody::Update(UpdateRsp::BlockSize(
                    size.try_into().unwrap_lite(),
                )))
            }
            ReqBody::Update(UpdateReq::Prep(target)) => {
                self.update.prep_image_update(target)?;
                Ok(RspBody::Ok)
            }
            ReqBody::Update(UpdateReq::WriteBlock { block_num }) => {
                self.update.write_one_block(block_num as usize, &req.blob)?;
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
