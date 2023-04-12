// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crc::{Crc, CRC_32_CKSUM};
use drv_sprot_api::{
    Protocol, ReqBody, Request, Response, RotIoStats, RspBody, SprocketsError,
    SprotError, SprotProtocolError, SprotStatus, UpdateReq, UpdateRsp,
    MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE,
};
use drv_update_api::{Update, UpdateStatus};
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
const_assert!(Protocol::V2 as u8 == 2);

/// State that is set once at the start of the driver
pub struct StartupState {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,
    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    /// TODO: This should live in the stage0 handoff info
    pub bootrom_crc32: u32,

    /// Maxiumum request size that the RoT can handle.
    pub max_request_size: u32,

    /// Maximum response size returned from the RoT to the SP
    pub max_response_size: u32,
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
                supported: 1_u32 << (Protocol::V2 as u8 - 1),
                bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
                max_request_size: MAX_REQUEST_SIZE.try_into().unwrap_lite(),
                max_response_size: MAX_RESPONSE_SIZE.try_into().unwrap_lite(),
            },
        }
    }

    /// Serialize and return a `SprotError::FlowError`
    pub fn flow_error(&self, tx_buf: &mut [u8]) -> usize {
        let body = Err(SprotProtocolError::FlowError.into());
        Response::pack(&body, tx_buf).unwrap_lite()
    }

    pub fn handle(
        &mut self,
        rx_buf: &[u8],
        tx_buf: &mut [u8],
        stats: &mut RotIoStats,
    ) -> usize {
        stats.rx_received = stats.rx_received.wrapping_add(1);
        let request = match Request::unpack(rx_buf) {
            Ok(request) => {
                ringbuf_entry!(Trace::Req {
                    protocol: rx_buf[0],
                    body_type: rx_buf[1]
                });
                request
            }
            Err(e) => {
                ringbuf_entry!(Trace::Err(e));
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                return Response::pack(&Err(e.into()), tx_buf).unwrap_lite();
            }
        };

        let rsp_body = self.handle_request(rx_buf, request, stats);
        Response::pack(&rsp_body, tx_buf).unwrap_lite()
    }

    pub fn handle_request(
        &mut self,
        rx_buf: &[u8],
        req: Request,
        stats: &mut RotIoStats,
    ) -> Result<RspBody, SprotError> {
        match req.body {
            ReqBody::Status => match self.update.status() {
                UpdateStatus::Rot(rot_updates) => {
                    let msg = SprotStatus {
                        supported: self.startup_state.supported,
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        max_request_size: self.startup_state.max_request_size,
                        max_response_size: self.startup_state.max_response_size,
                        rot_updates,
                    };
                    Ok(RspBody::Status(msg))
                }
                _ => {
                    stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                    Err(SprotProtocolError::BadUpdateStatus)?
                }
            },
            ReqBody::IoStats => Ok(RspBody::IoStats(stats.clone())),
            ReqBody::Sprockets(req) => Ok(RspBody::Sprockets(
                // The only error we can get here is a serialization error,
                // which is represented as `BadEncoding`.
                self.sprocket
                    .handle_deserialized(req)
                    .map_err(|_| SprocketsError::BadEncoding)?,
            )),
            ReqBody::Dump { addr } => {
                ringbuf_entry!(Trace::Dump(addr));
                let dumper = Dumper::from(DUMPER.get_task_id());
                dumper.dump(addr).map_err(SprotError::Dump)?;
                Ok(RspBody::Ok)
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
                match req.blob {
                    Some(blob) => {
                        let end = blob.offset + blob.size;
                        let block = &rx_buf[blob.offset..end];
                        self.update
                            .write_one_block(block_num as usize, &block)?;
                    }
                    _ => return Err(SprotProtocolError::MissingBlob)?,
                }
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
