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
        tx_buf: &mut [u8],
        status: &mut Status, // for responses and updating
    ) -> Option<usize> {
        let tx_payload = payload_buf_mut(None, tx_buf);
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
                            tx_payload[0] = MsgError::FlowError as u8;
                            ringbuf_entry!(Trace::Prev(self.count, self.prev));
                            self.prev = PrevMsg::Overrun;
                            ringbuf_entry!(Trace::ErrHeader(
                                self.count, self.prev, rx_buf[0], rx_buf[1],
                                rx_buf[2], rx_buf[3]
                            ));
                            return compose(MsgType::ErrorRsp, 1, tx_buf).ok();
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
            tx_payload[0] = MsgError::BadMessageLength as u8;
            return compose(MsgType::ErrorRsp, 1, tx_buf).ok();
        }

        // Parse the header which also checks the CRC.
        let (msgtype, rx_payload) = match parse(rx_buf) {
            Ok((msgtype, payload)) => {
                self.prev = PrevMsg::Good(msgtype);
                (msgtype, payload)
            }
            Err(msgerr) => {
                if msgerr == MsgError::NoMessage {
                    self.prev = PrevMsg::None;
                    return None;
                }
                ringbuf_entry!(Trace::ErrHeader(
                    self.count, self.prev, rx_buf[0], rx_buf[1], rx_buf[2],
                    rx_buf[3]
                ));
                tx_payload[0] = msgerr as u8;
                return compose(MsgType::ErrorRsp, 1, tx_buf).ok();
            }
        };

        // At this point, the header and payload are known to be
        // consistent with the CRC and the length is known to be good.

        // A message arrived intact. Look inside.
        let r: Result<(MsgType, usize), MsgError> = {
            // A message arrived intact
            status.rx_received = status.rx_received.wrapping_add(1);
            // The CRC validate header and range checked length can be trusted now.
            match msgtype {
                MsgType::EchoReq => {
                    if rx_payload.is_empty() {
                        Ok((MsgType::EchoRsp, 0))
                    } else if let Some(dst) =
                        tx_payload.get_mut(0..rx_payload.len())
                    {
                        dst.copy_from_slice(rx_payload);
                        Ok((MsgType::EchoRsp, dst.len()))
                    } else {
                        Err(MsgError::BadMessageLength)
                    }
                }
                MsgType::StatusReq => hubpack::serialize(tx_payload, &status)
                    .map_or(Err(MsgError::Serialization), |size| {
                        Ok((MsgType::StatusRsp, size))
                    }),
                MsgType::SprocketsReq => {
                    match self.sprocket.handle(rx_payload, tx_payload) {
                        Ok(size) => Ok((MsgType::SprocketsRsp, size)),
                        Err(_) => Ok((
                            MsgType::SprocketsRsp,
                            crate::handler::sprockets::bad_encoding_rsp(
                                tx_payload,
                            ),
                        )),
                    }
                }

                MsgType::UpdBlockSizeReq => {
                    let rsp = match self.update.block_size() {
                        Ok(block_size) => {
                            UpdateRspHeader::new(Some(block_size as u32), None)
                        }
                        Err(err) => {
                            UpdateRspHeader::new(None, Some(err.into()))
                        }
                    };
                    hubpack::serialize(tx_payload, &rsp)
                        .map_or(Err(MsgError::Serialization), |size| {
                            Ok((MsgType::UpdBlockSizeRsp, size))
                        })
                }
                MsgType::UpdPrepImageUpdateReq => {
                    match hubpack::deserialize::<UpdateTarget>(rx_payload) {
                        Ok((image_type, _n)) => {
                            match self.update.prep_image_update(image_type) {
                                Ok(()) => {
                                    let rsp = UpdateRspHeader::new(None, None);
                                    if let Ok(size) =
                                        hubpack::serialize(tx_payload, &rsp)
                                    {
                                        Ok((
                                            MsgType::UpdPrepImageUpdateRsp,
                                            size,
                                        ))
                                    } else {
                                        Err(MsgError::Serialization)
                                    }
                                }
                                Err(err) => {
                                    let rsp = UpdateRspHeader::new(
                                        None,
                                        Some(err.into()),
                                    );
                                    if let Ok(size) =
                                        hubpack::serialize(tx_payload, &rsp)
                                    {
                                        Ok((
                                            MsgType::UpdPrepImageUpdateRsp,
                                            size,
                                        ))
                                    } else {
                                        Err(MsgError::Serialization)
                                    }
                                }
                            }
                        }
                        Err(_err) => Err(MsgError::Serialization),
                    }
                }
                MsgType::UpdWriteOneBlockReq => {
                    match hubpack::deserialize::<u32>(rx_payload) {
                        Ok((block_num, block)) => {
                            match self
                                .update
                                .write_one_block(block_num as usize, block)
                            {
                                Ok(()) => {
                                    let rsp = UpdateRspHeader::new(None, None);
                                    hubpack::serialize(tx_payload, &rsp).map_or(
                                        Err(MsgError::Serialization),
                                        |size| {
                                            Ok((
                                                MsgType::UpdWriteOneBlockRsp,
                                                size,
                                            ))
                                        },
                                    )
                                }
                                Err(_err) => Err(MsgError::Serialization),
                            }
                        }
                        Err(_err) => Err(MsgError::Serialization),
                    }
                }

                MsgType::UpdAbortUpdateReq => {
                    match self.update.abort_update() {
                        Ok(()) => {
                            let rsp = UpdateRspHeader::new(None, None);
                            hubpack::serialize(tx_payload, &rsp)
                                .map_or(Err(MsgError::Serialization), |size| {
                                    Ok((MsgType::UpdAbortUpdateRsp, size))
                                })
                        }
                        Err(_err) => Err(MsgError::Serialization),
                    }
                }
                MsgType::UpdFinishImageUpdateReq => {
                    match self.update.finish_image_update() {
                        Ok(()) => {
                            let rsp = UpdateRspHeader::new(None, None);
                            hubpack::serialize(tx_payload, &rsp).map_or(
                                Err(MsgError::Serialization),
                                |size| {
                                    Ok((MsgType::UpdFinishImageUpdateRsp, size))
                                },
                            )
                        }
                        Err(_err) => Err(MsgError::Serialization),
                    }
                }
                MsgType::UpdCurrentVersionReq => {
                    let rsp = self.update.current_version();
                    hubpack::serialize(tx_payload, &rsp)
                        .map_or(Err(MsgError::Serialization), |size| {
                            Ok((MsgType::UpdCurrentVersionRsp, size))
                        })
                }
                MsgType::SinkReq => {
                    // The first two bytes of a SinkReq payload are the U16
                    // mod 2^16 sequence number.
                    tx_payload[0..2].copy_from_slice(&rx_payload[0..2]);
                    Ok((MsgType::SinkRsp, 2))
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
                    Err(MsgError::BadMessageType)
                }
            }
        };
        // The above cases either enqueued a message and returned size
        // or generated 1-byte error code.
        match r {
            Ok((msgtype, payload_size)) => {
                compose(msgtype, payload_size, tx_buf).ok()
            }
            Err(err) if err == MsgError::NoMessage => None,
            Err(err) => {
                tx_payload[0] = err as u8;
                compose(MsgType::ErrorRsp, 1, tx_buf).ok()
            }
        }
    }
}
