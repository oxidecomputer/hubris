// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use attest_api::Attest;
use crc::{Crc, CRC_32_CKSUM};
use drv_lpc55_update_api::{RotPage, SlotId, Update};
use drv_sp_ctrl_api::SpCtrl;
use drv_sprot_api::{
    AttestReq, AttestRsp, CabooseReq, CabooseRsp, DumpReq, DumpRsp, ReqBody,
    Request, Response, RotIoStats, RotPageRsp, RotState, RotStatus, RspBody,
    SprocketsError, SprotError, SprotProtocolError, UpdateReq, UpdateRsp,
    WatchdogError, CURRENT_VERSION, MIN_VERSION, REQUEST_BUF_SIZE,
    RESPONSE_BUF_SIZE,
};
use dumper_api::Dumper;
use lpc55_romapi::bootrom;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use sprockets_rot::RotSprocket;
use userlib::{task_slot, UnwrapLite};

mod sprockets;

task_slot!(UPDATE_SERVER, update_server);

task_slot!(DUMPER, dumper);

task_slot!(ATTEST, attest);

task_slot!(SP_CTRL, swd);

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
pub enum TrailingData<'a> {
    Caboose { slot: SlotId, start: u32, size: u32 },
    AttestCert { index: u32, offset: u32, size: u32 },
    AttestLog { offset: u32, size: u32 },
    Attest { nonce: &'a [u8], write_size: u32 },
    RotPage { page: RotPage },
}

pub struct Handler {
    sprocket: RotSprocket,
    update: Update,
    startup_state: StartupState,
    attest: Attest,
    sp_ctrl: SpCtrl,
}

impl<'a> Handler {
    pub fn new() -> Handler {
        Handler {
            sprocket: crate::handler::sprockets::init(),
            update: Update::from(UPDATE_SERVER.get_task_id()),
            startup_state: StartupState {
                bootrom_crc32: CRC32.checksum(&bootrom().data[..]),
                max_request_size: REQUEST_BUF_SIZE.try_into().unwrap_lite(),
                max_response_size: RESPONSE_BUF_SIZE.try_into().unwrap_lite(),
            },
            attest: Attest::from(ATTEST.get_task_id()),
            sp_ctrl: SpCtrl::from(SP_CTRL.get_task_id()),
        }
    }

    pub fn flow_error(&self, tx_buf: &mut [u8; RESPONSE_BUF_SIZE]) -> usize {
        let body = Err(SprotProtocolError::FlowError.into());
        Response::pack(&body, tx_buf)
    }

    pub fn desynchronized_error(
        &self,
        tx_buf: &mut [u8; RESPONSE_BUF_SIZE],
    ) -> usize {
        let body = Err(SprotProtocolError::Desynchronized.into());
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
        match trailer {
            Some(TrailingData::Caboose {
                slot,
                start,
                size: blob_size,
            }) => {
                let blob_size: usize = blob_size.try_into().unwrap_lite();
                if blob_size > drv_sprot_api::MAX_BLOB_SIZE {
                    // If there isn't enough room, then pack an error instead
                    Response::pack(
                        &Err(SprotError::Protocol(
                            SprotProtocolError::BadMessageLength,
                        )),
                        tx_buf,
                    )
                } else {
                    let pack_result =
                        Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                            self.update
                                .read_raw_caboose(
                                    slot,
                                    start,
                                    &mut buf[..blob_size],
                                )
                                .map_err(|e| RspBody::Caboose(Err(e)))?;
                            Ok(blob_size)
                        });
                    match pack_result {
                        Ok(size) => size,
                        Err(e) => Response::pack(&Ok(e), tx_buf),
                    }
                }
            }
            Some(TrailingData::AttestCert {
                index,
                offset,
                size,
            }) => {
                let size: usize = usize::try_from(size).unwrap_lite();
                if size > drv_sprot_api::MAX_BLOB_SIZE {
                    Response::pack(
                        &Err(SprotError::Protocol(
                            SprotProtocolError::BadMessageLength,
                        )),
                        tx_buf,
                    )
                } else {
                    let pack_result =
                        Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                            self.attest
                                .cert(index, offset, &mut buf[..size])
                                .map_err(|e| RspBody::Attest(Err(e)))?;
                            Ok(size)
                        });
                    match pack_result {
                        Ok(size) => size,
                        Err(e) => Response::pack(&Ok(e), tx_buf),
                    }
                }
            }
            Some(TrailingData::RotPage { page }) => {
                let size: usize = lpc55_rom_data::FLASH_PAGE_SIZE;
                static_assertions::const_assert!(
                    lpc55_rom_data::FLASH_PAGE_SIZE
                        <= drv_sprot_api::MAX_BLOB_SIZE
                );
                let pack_result =
                    Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                        self.update
                            .read_rot_page(page, &mut buf[..size])
                            .map_err(|e| RspBody::Page(Err(e)))?;
                        Ok(size)
                    });
                match pack_result {
                    Ok(size) => size,
                    Err(e) => Response::pack(&Ok(e), tx_buf),
                }
            }
            Some(TrailingData::AttestLog { offset, size }) => {
                let size: usize = usize::try_from(size).unwrap_lite();
                if size > drv_sprot_api::MAX_BLOB_SIZE {
                    Response::pack(
                        &Err(SprotError::Protocol(
                            SprotProtocolError::BadMessageLength,
                        )),
                        tx_buf,
                    )
                } else {
                    let pack_result =
                        Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                            self.attest
                                .log(offset, &mut buf[..size])
                                .map_err(|e| RspBody::Attest(Err(e)))?;
                            Ok(size)
                        });
                    match pack_result {
                        Ok(size) => size,
                        Err(e) => Response::pack(&Ok(e), tx_buf),
                    }
                }
            }
            Some(TrailingData::Attest { nonce, write_size }) => {
                if write_size as usize > drv_sprot_api::MAX_BLOB_SIZE {
                    Response::pack(
                        &Err(SprotError::Protocol(
                            SprotProtocolError::BadMessageLength,
                        )),
                        tx_buf,
                    )
                } else {
                    let pack_result =
                        Response::pack_with_cb(&rsp_body, tx_buf, |buf| {
                            self.attest
                                .attest(nonce, &mut buf[..write_size as usize])
                                .map_err(|e| RspBody::Attest(Err(e)))?;
                            Ok(write_size as usize)
                        });
                    match pack_result {
                        Ok(size) => size,
                        Err(e) => Response::pack(&Ok(e), tx_buf),
                    }
                }
            }
            _ => Response::pack(&rsp_body, tx_buf),
        }
    }

    pub fn handle_request(
        &mut self,
        req: Request<'a>,
        stats: &mut RotIoStats,
    ) -> Result<(RspBody, Option<TrailingData<'a>>), SprotError> {
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
            ReqBody::IoStats => Ok((RspBody::IoStats(*stats), None)),
            ReqBody::RotState => match self.update.status() {
                Ok(state) => {
                    let msg = RotState::V1 {
                        bootrom_crc32: self.startup_state.bootrom_crc32,
                        state,
                    };
                    Ok((RspBody::RotState(msg), None))
                }
                Err(_e) => {
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
                ringbuf_entry!(Trace::Dump(addr));
                let dumper = Dumper::from(DUMPER.get_task_id());
                let err = dumper.dump(addr).err();
                Ok((RspBody::Dump(DumpRsp::V1 { err }), None))
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
                self.update.write_one_block(block_num as usize, req.blob)?;
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
                        Err(e) => Err(e),
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
            ReqBody::Update(UpdateReq::BootInfo) => {
                let boot_info = self.update.rot_boot_info()?;
                Ok((RspBody::Update(boot_info.into()), None))
            }
            ReqBody::Attest(AttestReq::Cert {
                index,
                offset,
                size,
            }) => {
                // This command returns a variable amount of data that belongs
                // in the trailing data region of the response. We return a
                // marker struct with the data necessary retrieve this data so
                // the work can be done elsewhere.
                Ok((
                    RspBody::Attest(Ok(AttestRsp::Cert)),
                    Some(TrailingData::AttestCert {
                        index,
                        offset,
                        size,
                    }),
                ))
            }
            ReqBody::Attest(AttestReq::CertChainLen) => {
                let rsp = match self.attest.cert_chain_len() {
                    Ok(v) => Ok(AttestRsp::CertChainLen(v)),
                    Err(e) => Err(e),
                };
                Ok((RspBody::Attest(rsp), None))
            }
            ReqBody::Attest(AttestReq::CertLen(i)) => {
                let rsp = match self.attest.cert_len(i) {
                    Ok(v) => Ok(AttestRsp::CertLen(v)),
                    Err(e) => Err(e),
                };
                Ok((RspBody::Attest(rsp), None))
            }
            ReqBody::Attest(AttestReq::Record { algorithm }) => {
                let rsp = match self.attest.record(algorithm, req.blob) {
                    Ok(()) => Ok(AttestRsp::Record),
                    Err(e) => Err(e),
                };
                Ok((RspBody::Attest(rsp), None))
            }
            ReqBody::RotPage { page } => {
                // This command returns a variable amount of data that belongs
                // in the trailing data region of the response. We return a
                // marker struct with the data necessary retrieve this data so
                // the work can be done elsewhere.
                Ok((
                    RspBody::Page(Ok(RotPageRsp::RotPage)),
                    Some(TrailingData::RotPage { page }),
                ))
            }
            ReqBody::Attest(AttestReq::Log { offset, size }) => Ok((
                RspBody::Attest(Ok(AttestRsp::Log)),
                Some(TrailingData::AttestLog { offset, size }),
            )),
            ReqBody::Attest(AttestReq::LogLen) => {
                let rsp = match self.attest.log_len() {
                    Ok(l) => Ok(AttestRsp::LogLen(l)),
                    Err(e) => Err(e),
                };
                Ok((RspBody::Attest(rsp), None))
            }
            ReqBody::Attest(AttestReq::Attest {
                nonce_size,
                write_size,
            }) => Ok((
                RspBody::Attest(Ok(AttestRsp::Attest)),
                Some(TrailingData::Attest {
                    nonce: &req.blob[..nonce_size as usize],
                    write_size,
                }),
            )),
            ReqBody::Attest(AttestReq::AttestLen) => {
                let rsp = match self.attest.attest_len() {
                    Ok(l) => Ok(AttestRsp::AttestLen(l)),
                    Err(e) => Err(e),
                };
                Ok((RspBody::Attest(rsp), None))
            }
            ReqBody::EnableSpSlotWatchdog { time_ms } => {
                // Enabling the watchdog doesn't actually do any SWD work, but
                // we'll call `setup()` now to make sure that the SWD system is
                // working.
                match self.sp_ctrl.setup().and_then(|()| {
                    self.sp_ctrl.enable_sp_slot_watchdog(time_ms)
                }) {
                    Ok(()) => Ok((RspBody::Ok, None)),
                    Err(_e) => Err(SprotError::Watchdog(WatchdogError::SpCtrl)),
                }
            }
            ReqBody::DisableSpSlotWatchdog => {
                self.sp_ctrl.disable_sp_slot_watchdog();
                Ok((RspBody::Ok, None))
            }
        }
    }
}
