// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crc::{Crc, CRC_32_CKSUM};
use drv_lpc55_update_api::{SlotId, Update, UpdateStatus};
use drv_sprot_api::{
    CabooseReq, CabooseRsp, DumpReq, DumpRsp, ReqBody, Request, Response,
    RotIoStats, RotState, RotStatus, RspBody, SprocketsError, SprotError,
    SprotProtocolError, UpdateReq, UpdateRsp, CURRENT_VERSION, MIN_VERSION,
    REQUEST_BUF_SIZE, RESPONSE_BUF_SIZE,
};
use dumper_api::Dumper;
use lpc55_romapi::bootrom;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use sprockets_rot::RotSprocket;
use userlib::{task_slot, UnwrapLite};

mod sprockets;

task_slot!(UPDATE_SERVER, update_server);

#[cfg(feature = "dumper")]
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

/// Marker for data which should be copied after the packet is encoded
pub enum TrailingData {
    Caboose { slot: SlotId, start: u32, size: u32 },
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
        let (rsp_body, trailer) = match Request::unpack(rx_buf) {
            Ok(request) => match self.handle_request(request, stats) {
                Ok((v, t)) => (Ok(v), t),
                Err(e) => (Err(e), None),
            },
            Err(e) => {
                ringbuf_entry!(Trace::Err(e));
                stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                (Err(e.into()), None)
            }
        };

        // In certain cases, handling the request has left us with trailing data
        // that needs to be packed into the remaining packet space.
        let size = match trailer {
            Some(TrailingData::Caboose {
                slot,
                start,
                size: blob_size,
            }) => {
                let blob_size: usize = blob_size.try_into().unwrap_lite();
                if blob_size as usize > drv_sprot_api::MAX_BLOB_SIZE {
                    // If there isn't enough room, then pack an error instead
                    Response::pack(
                        &Err(SprotError::Protocol(
                            SprotProtocolError::BadMessageLength,
                        )),
                        tx_buf,
                    )
                } else {
                    match Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                        self.update
                            .read_raw_caboose(
                                slot,
                                start,
                                &mut buf[..blob_size as usize],
                            )
                            .map_err(|e| RspBody::Caboose(Err(e.into())))?;
                        Ok(blob_size)
                    }) {
                        Ok(size) => size,
                        Err(e) => Response::pack(&Ok(e), tx_buf),
                    }
                }
            }
            _ => Response::pack(&rsp_body, tx_buf),
        };

        size
    }

    pub fn handle_request(
        &mut self,
        req: Request,
        stats: &mut RotIoStats,
    ) -> Result<(RspBody, Option<TrailingData>), SprotError> {
        match req.body {
            ReqBody::Status => {
                let status = RotStatus {
                    version: CURRENT_VERSION,
                    min_version: MIN_VERSION,
                    request_buf_size: self.startup_state.max_request_size,
                    response_buf_size: self.startup_state.max_response_size,
                };
                Ok((RspBody::Status(status), None))
            }
            ReqBody::IoStats => Ok((RspBody::IoStats(stats.clone()), None)),
            ReqBody::RotState => match self.update.status() {
                UpdateStatus::Rot(state) => {
                    let msg = RotState::V1 {
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        state,
                    };
                    Ok((RspBody::RotState(msg), None))
                }
                _ => {
                    stats.rx_invalid = stats.rx_invalid.wrapping_add(1);
                    Err(SprotProtocolError::BadUpdateStatus)?
                }
            },
            ReqBody::Sprockets(req) => Ok((
                RspBody::Sprockets(
                    // The only error we can get here is a serialization error,
                    // which is represented as `BadEncoding`.
                    self.sprocket
                        .handle_deserialized(req)
                        .map_err(|_| SprocketsError::BadEncoding)?,
                ),
                None,
            )),
            ReqBody::Dump(DumpReq::V1 { addr }) => {
                #[cfg(feature = "dumper")]
                {
                    ringbuf_entry!(Trace::Dump(addr));
                    let dumper = Dumper::from(DUMPER.get_task_id());
                    let err = dumper.dump(addr).err();
                    Ok((RspBody::Dump(DumpRsp::V1 { err }), None))
                }
                #[cfg(not(feature = "dumper"))]
                Ok((
                    RspBody::Dump(DumpRsp::V1 {
                        err: Some(dumper_api::DumperError::SetupFailed),
                    }),
                    None,
                ))
            }
            ReqBody::Update(UpdateReq::GetBlockSize) => {
                let size = self.update.block_size()?;
                // Block size will always fit in a u32 on these MCUs
                Ok((
                    RspBody::Update(UpdateRsp::BlockSize(
                        size.try_into().unwrap_lite(),
                    )),
                    None,
                ))
            }
            ReqBody::Update(UpdateReq::Prep(target)) => {
                self.update.prep_image_update(target)?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Update(UpdateReq::WriteBlock { block_num }) => {
                self.update.write_one_block(block_num as usize, &req.blob)?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Update(UpdateReq::Abort) => {
                self.update.abort_update()?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Update(UpdateReq::Finish) => {
                self.update.finish_image_update()?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Update(UpdateReq::SwitchDefaultImage {
                slot,
                duration,
            }) => {
                self.update.switch_default_image(slot, duration)?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Update(UpdateReq::Reset) => {
                self.update.reset()?;
                Ok((RspBody::Ok, None))
            }
            ReqBody::Caboose(c) => match c {
                CabooseReq::Size { slot } => {
                    let rsp = match self.update.caboose_size(slot) {
                        Ok(v) => Ok(CabooseRsp::Size(v)),
                        Err(e) => Err(e.into()),
                    };
                    Ok((RspBody::Caboose(rsp), None))
                }
                CabooseReq::Read { slot, start, size } => {
                    // In this case, we're going to be sending back a variable
                    // amount of data in the trailing section of the packet.  We
                    // don't know exactly where that data will be placed, so
                    // we'll return a marker here and copy it later.
                    Ok((
                        RspBody::Caboose(Ok(CabooseRsp::Read)),
                        Some(TrailingData::Caboose { slot, start, size }),
                    ))
                }
            },
        }
    }
}
