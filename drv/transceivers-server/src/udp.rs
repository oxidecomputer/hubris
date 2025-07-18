// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hardware revision-independent server code for UDP interactions
//!
//! This is in a separate module to avoid polluting `main.rs` with a bunch of
//! imports from the `transceiver_messages` crate; it simply adds more functions
//! to our existing `ServerImpl`.
//!
//! All of the API types in `transceiver_messages` operate on **physical**
//! ports, i.e. an FPGA paired by a physical port index (or mask).
//!
use counters::Count;
use hubpack::SerializedSize;
use ringbuf::*;
use userlib::UnwrapLite;

use crate::{FrontIOStatus, ServerImpl};
use drv_front_io_api::{
    transceivers::{
        FpgaI2CFailure, LogicalPort, LogicalPortFailureTypes, LogicalPortMask,
        ModuleResult, ModuleResultNoFailure, ModuleResultSlim,
    },
    FrontIOError,
};
use task_net_api::*;
use transceiver_messages::{
    mac::MacAddrs,
    message::*,
    mgmt::{ManagementInterface, MemoryRead, MemoryWrite, Page},
    ModuleId,
};

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,
    DeserializeError(hubpack::Error),
    DeserializeHeaderError(hubpack::Error),
    SendError(SendError),
    AssertReset(ModuleId),
    DeassertReset(ModuleId),
    AssertLpMode(ModuleId),
    DeassertLpMode(ModuleId),
    EnablePower(ModuleId),
    DisablePower(ModuleId),
    Status(ModuleId),
    ExtendedStatus(ModuleId),
    Read(ModuleId, MemoryRead),
    Write(ModuleId, MemoryWrite),
    ManagementInterface(ManagementInterface),
    UnexpectedHostResponse(HostResponse),
    GotSpRequest,
    GotSpResponse,
    WrongVersion(u8),
    MacAddrs,
    GotError(ProtocolError),
    ResponseSize(ResponseSize),
    OperationResult(ModuleResultSlim),
    OperationNoFailResult(ModuleResultNoFailure),
    ClearPowerFault(ModuleId),
    LedState(ModuleId),
    SetLedState(ModuleId, LedState),
    ClearDisableLatch(ModuleId),
    PageSelectI2CFailures(LogicalPort, FpgaI2CFailure),
    ReadI2CFailures(LogicalPort, FpgaI2CFailure),
    WriteI2CFailures(LogicalPort, FpgaI2CFailure),
    FrontIOError(FrontIOError),
}

counted_ringbuf!(Trace, 32, Trace::None);

////////////////////////////////////////////////////////////////////////////////
#[derive(Copy, Clone, PartialEq)]
struct ResponseSize {
    header_length: u8,
    message_length: u8,
    data_length: u16,
}

impl ServerImpl {
    /// Attempt to read and handle data from the `net` socket
    pub fn check_net(
        &mut self,
        rx_data_buf: &mut [u8],
        tx_data_buf: &mut [u8],
    ) {
        match self.net.recv_packet(
            SocketName::transceivers,
            LargePayloadBehavior::Discard,
            rx_data_buf,
        ) {
            Ok(meta) => self.handle_packet(meta, rx_data_buf, tx_data_buf),
            Err(RecvError::QueueEmpty | RecvError::ServerRestarted) => {
                // Our incoming queue is empty or `net` restarted. Wait for more
                // packets in dispatch, back in the main loop.
            }
        }
    }

    fn handle_packet(
        &mut self,
        mut meta: UdpMetadata,
        rx_data_buf: &[u8],
        tx_data_buf: &mut [u8],
    ) {
        let out_len =
            // attempt to deserialize the header
            match hubpack::deserialize::<Header>(&rx_data_buf[..meta.size as usize]) {
                Ok((header, request)) => {
                    // header deserialized successfully, so now attempt to
                    // deserialize the remaining message
                    match hubpack::deserialize::<Message>(request) {
                        // handle the message
                        Ok((msg, data)) => {
                            self.handle_message(msg, header.message_id, data, tx_data_buf)
                        },
                        // try to tell the host something useful about what happened
                        Err(e) => {
                            ringbuf_entry!(Trace::DeserializeError(e));
                            self.handle_deserialization_error(header, tx_data_buf, request)
                        },
                    }
                }

                // nothing we can do if we cannot even deserialize the header
                Err(e) => {
                    ringbuf_entry!(Trace::DeserializeHeaderError(e));
                    None
                }
            };

        if let Some(out_len) = out_len {
            // Modify meta.size based on the output packet size
            meta.size = out_len;
            if let Err(e) = self.net.send_packet(
                SocketName::transceivers,
                meta,
                &tx_data_buf[..meta.size as usize],
            ) {
                ringbuf_entry!(Trace::SendError(e));
                // Yeah this match looks useless, but (1) it guards against new
                // variants being added to the SendError type, and (2) it
                // provides a convenient place to hang comments.
                match e {
                    // We'll drop packets if the outgoing queue is full;
                    // the host is responsible for retrying.
                    SendError::QueueFull => (),
                    // For lack of a better idea, we will also drop packets if
                    // the netstack restarts -- since the host has to have the
                    // capacity to retry anyway. The main alternative is to
                    // retry, but on discussion we felt it wasn't worth it --
                    // and besides, it creates a potential for a CPU-burning
                    // retry loop if the netstack were to crashloop, Jefe
                    // forbid.
                    SendError::ServerRestarted => (),
                }
            }
        }
    }

    /// At this point, Message deserialization has failed, so we can't handle
    /// the packet. We'll look at *just the header* (which should never change),
    /// in the hopes of logging a more detailed error message about a version
    /// mismatch. We'll also return a `ProtocolError` message to the host
    /// (since we've got a packet number).
    fn handle_deserialization_error(
        &mut self,
        header: Header,
        tx_data_buf: &mut [u8],
        request: &[u8],
    ) -> Option<u32> {
        // This message comes from a host implementation older than the
        // minimum committed version of the protocol. We really can't do
        // anything with this message, and the protocol mandates that we
        // drop the message.
        if header.version() < version::outer::MIN {
            ringbuf_entry!(Trace::WrongVersion(header.version()));
            None

        // In this case, the message has failed to deserialize, but
        // we've successfully deserialized the header. That implies that
        // the host has sent us a message that does not exist in our own
        // implementation of the protocol.
        //
        // Check that implication by ensuring that the host version is
        // after our own CURRENT. To do this, we use our knowledge that version
        // field is the next byte in the request.
        } else if request[0] > version::inner::CURRENT {
            let header_size = hubpack::serialize(
                tx_data_buf,
                &Header::new(header.message_id, MessageKind::Error),
            )
            .unwrap();
            let message_size = hubpack::serialize(
                &mut tx_data_buf[header_size..],
                &Message::new(MessageBody::Error(
                    ProtocolError::VersionMismatch {
                        expected: version::outer::CURRENT,
                        actual: header.version(),
                    },
                )),
            )
            .unwrap();
            Some((header_size + message_size) as u32)

        // This last case catches failures to deserialize something we
        // _should_ have been able to deserialize. The host version is
        // between MIN and CURRENT, so it's a message that's part of our
        // own protocol. We really just failed to deserialize it, such
        // as a corrupt buffer or some similar failure mode. Just drop
        // the message, since we've already logged a `DeserializeError`.
        } else {
            None
        }
    }

    /// Handles a single message from the host with supplementary data in `data`
    ///
    /// Returns a response and a `usize` indicating how much was written to the
    /// `out` buffer.
    fn handle_message(
        &mut self,
        msg: Message,
        msg_id: u64,
        data: &[u8],
        tx_data_buf: &mut [u8],
    ) -> Option<u32> {
        // If the version is below the minimum committed, then we can't really
        // trust the message, even though it nominally deserialized correctly.
        // Don't reply.
        if msg.version() < version::inner::MIN {
            ringbuf_entry!(Trace::WrongVersion(msg.version()));
            return None;
        }

        let reserved_framing = Header::MAX_SIZE + Message::MAX_SIZE;
        let (body, data_len) = match msg.body {
            // These messages should never be sent to us, and we reply
            // with a `WrongMessage` below.
            MessageBody::SpRequest(..) => {
                ringbuf_entry!(Trace::GotSpRequest);
                (MessageBody::Error(ProtocolError::WrongMessage), 0)
            }
            MessageBody::SpResponse(..) => {
                ringbuf_entry!(Trace::GotSpResponse);
                (MessageBody::Error(ProtocolError::WrongMessage), 0)
            }
            // Nothing implemented yet
            MessageBody::HostResponse(r) => {
                ringbuf_entry!(Trace::UnexpectedHostResponse(r));
                return None;
            }
            // Happy path: the host is asking something of us!
            MessageBody::HostRequest(h) => self.handle_host_request(
                h,
                data,
                &mut tx_data_buf[reserved_framing..],
            ),
            // Nothing implemented yet
            MessageBody::Error(e) => {
                ringbuf_entry!(Trace::GotError(e));
                return None;
            }
        };

        // Serialize the Header into the front of the tx buffer, followed by
        // the actual Message. Any payload data was already written into the
        // back part of the tx_data_buf by `handle_host_request()`. This sends
        // out _our_ protocol version number which may differ from the host's,
        // but should be compatible with it.
        let response = Message::new(body);
        let header = Header::new(msg_id, response.kind());

        let hdr_len = hubpack::serialize(tx_data_buf, &header).unwrap();
        let msg_len =
            hubpack::serialize(&mut tx_data_buf[hdr_len..], &response).unwrap();

        ringbuf_entry!(Trace::ResponseSize(ResponseSize {
            header_length: hdr_len as u8,
            message_length: msg_len as u8,
            data_length: data_len as u16
        }));

        // At this point, any supplementary data was written to
        // `tx_data_buf[Header::MAX_SIZE + Message::MAX_SIZE..]`, so it's not
        // necessarily tightly packed against the end of the `Header` and
        // `Message`. Let's shift it backwards based on the size of that leading
        // data.
        tx_data_buf.copy_within(
            reserved_framing..(reserved_framing + data_len),
            hdr_len + msg_len,
        );
        Some((hdr_len + msg_len + data_len) as u32)
    }

    /// Constructs a MessageBody and provides the length of payload data
    ///
    /// The `transceiver-message` protocol supports providing dynamic amounts of
    /// return data. All responses may contain failure information (up to one
    /// `HwError` per module), even if there is no additional payload data
    /// expected. We must ensure that we do not overflow the `out` buffer by
    /// keeping the data written under `transceiver_messages::MAX_PAYLOAD_SIZE`.
    fn handle_host_request(
        &mut self,
        h: HostRequest,
        data: &[u8],
        out: &mut [u8],
    ) -> (MessageBody, usize) {
        match h {
            HostRequest::Status(modules) => {
                ringbuf_entry!(Trace::Status(modules));
                let mask = LogicalPortMask::from(modules);
                let (data_len, result) = if self.front_io.board_ready() {
                    self.get_status(mask, out)
                } else {
                    (
                        0,
                        ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                            .unwrap_lite(),
                    )
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (err_len, errored_modules) =
                    self.handle_errors(modules, result, &mut out[data_len..]);
                let final_payload_len = data_len + err_len;

                (
                    MessageBody::SpResponse(SpResponse::Status {
                        modules: success,
                        failed_modules: errored_modules,
                    }),
                    final_payload_len,
                )
            }
            HostRequest::ExtendedStatus(modules) => {
                ringbuf_entry!(Trace::ExtendedStatus(modules));
                let mask = LogicalPortMask::from(modules);
                let (data_len, result) = if self.front_io.board_ready() {
                    self.get_extended_status(mask, out)
                } else {
                    (
                        0,
                        ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                            .unwrap_lite(),
                    )
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (err_len, errored_modules) =
                    self.handle_errors(modules, result, &mut out[data_len..]);
                let final_payload_len = data_len + err_len;

                (
                    MessageBody::SpResponse(SpResponse::ExtendedStatus {
                        modules: success,
                        failed_modules: errored_modules,
                    }),
                    final_payload_len,
                )
            }
            HostRequest::Read { modules, read } => {
                ringbuf_entry!(Trace::Read(modules, read));
                // The host is not setting the the upper 32 bits at this time,
                // but should that happen we need to know how many HwErrors we
                // will serialize due to invalid modules being specified. We
                // only precalculate error length requirements in Read as that
                // is the only message whose response can approach
                // MAX_PAYLOAD_SIZE.
                let num_invalid = ModuleId(modules.0 & 0xffffffff00000000)
                    .selected_transceiver_count();
                let mask = LogicalPortMask::from(modules);
                let num_disabled = (mask & self.disabled).count();
                let read_data = read.len() as usize * mask.count();
                let module_err =
                    HwError::MAX_SIZE * (num_invalid + num_disabled);
                if read_data + module_err
                    > transceiver_messages::MAX_PAYLOAD_SIZE
                {
                    return (
                        MessageBody::Error(ProtocolError::RequestTooLarge),
                        0,
                    );
                }

                let (data_len, result) = if self.front_io.board_ready() {
                    let r = self.read(read, mask & !self.disabled, out);
                    let len = r.success().count() * read.len() as usize;
                    (len, r)
                } else {
                    (
                        0,
                        ModuleResult::new(
                            LogicalPortMask(0),
                            LogicalPortMask(0),
                            mask,
                            LogicalPortFailureTypes::default(),
                        )
                        .unwrap_lite(),
                    )
                };

                ringbuf_entry!(Trace::OperationResult(result.to_slim()));
                let success = ModuleId::from(result.success());
                let (err_len, failed_modules) = self
                    .handle_errors_and_failures_and_disabled(
                        modules,
                        result,
                        &mut out[data_len..],
                    );
                let final_payload_len = data_len + err_len;

                (
                    MessageBody::SpResponse(SpResponse::Read {
                        modules: success,
                        failed_modules,
                        read,
                    }),
                    final_payload_len,
                )
            }
            HostRequest::Write { modules, write } => {
                ringbuf_entry!(Trace::Write(modules, write));
                if write.len() as usize != data.len() {
                    return (
                        MessageBody::Error(ProtocolError::WrongDataSize {
                            expected: write.len() as u32,
                            actual: data.len() as u32,
                        }),
                        0,
                    );
                }

                let mask = LogicalPortMask::from(modules);
                let result = if self.front_io.board_ready() {
                    self.write(write, mask & !self.disabled, data)
                } else {
                    ModuleResult::new(
                        LogicalPortMask(0),
                        LogicalPortMask(0),
                        mask,
                        LogicalPortFailureTypes::default(),
                    )
                    .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationResult(result.to_slim()));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) = self
                    .handle_errors_and_failures_and_disabled(
                        modules, result, out,
                    );

                (
                    MessageBody::SpResponse(SpResponse::Write {
                        modules: success,
                        failed_modules,
                        write,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::AssertReset(modules) => {
                ringbuf_entry!(Trace::AssertReset(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_assert_reset(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::DeassertReset(modules) => {
                ringbuf_entry!(Trace::DeassertReset(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_deassert_reset(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::AssertLpMode(modules) => {
                ringbuf_entry!(Trace::AssertLpMode(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_assert_lpmode(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::DeassertLpMode(modules) => {
                ringbuf_entry!(Trace::DeassertLpMode(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_deassert_lpmode(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::EnablePower(modules) => {
                ringbuf_entry!(Trace::EnablePower(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_enable_power(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::DisablePower(modules) => {
                ringbuf_entry!(Trace::DisablePower(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_disable_power(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::ManagementInterface {
                modules: _,
                interface: i,
            } => {
                // TODO: Implement this
                ringbuf_entry!(Trace::ManagementInterface(i));
                (MessageBody::Error(ProtocolError::NotSupported), 0)
            }
            HostRequest::MacAddrs => {
                ringbuf_entry!(Trace::MacAddrs);
                let b = self.net.get_spare_mac_addresses();
                match MacAddrs::new(b.base_mac, b.count.get(), b.stride) {
                    Ok(out) => (
                        MessageBody::SpResponse(SpResponse::MacAddrs(
                            MacAddrResponse::Ok(out),
                        )),
                        0,
                    ),
                    Err(e) => (
                        MessageBody::SpResponse(SpResponse::MacAddrs(
                            MacAddrResponse::Error(e),
                        )),
                        0,
                    ),
                }
            }
            HostRequest::ClearPowerFault(modules) => {
                ringbuf_entry!(Trace::ClearPowerFault(modules));
                let mask = LogicalPortMask::from(modules) & !self.disabled;

                let result = if self.front_io.board_ready() {
                    self.front_io.transceivers_clear_power_fault(mask)
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                ringbuf_entry!(Trace::OperationNoFailResult(result));
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors_and_disabled(modules, result, out);

                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::LedState(modules) => {
                ringbuf_entry!(Trace::LedState(modules));
                let mask = LogicalPortMask::from(modules);
                let (data_len, result) = if self.front_io.board_ready() {
                    self.get_led_state_response(mask, out)
                } else {
                    (
                        0,
                        ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                            .unwrap_lite(),
                    )
                };

                // This operation just queries internal SP state, so it is
                // always successful. However, invalid modules may been
                // specified by the host so we need to check that anyway.
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors(modules, result, &mut out[data_len..]);
                (
                    MessageBody::SpResponse(SpResponse::LedState {
                        modules: success,
                        failed_modules,
                    }),
                    data_len + num_err_bytes,
                )
            }
            HostRequest::SetLedState { modules, state } => {
                ringbuf_entry!(Trace::SetLedState(modules, state));
                let mask = LogicalPortMask::from(modules);

                let result = if self.front_io.board_ready() {
                    self.front_io.led_set_state(mask, state);
                    ModuleResultNoFailure::new(mask, LogicalPortMask(0))
                        .unwrap_lite()
                } else {
                    ModuleResultNoFailure::new(LogicalPortMask(0), mask)
                        .unwrap_lite()
                };

                // This operation just sets internal SP state, so it is always
                // successful. However, invalid modules may been specified by
                // the host so we need to check that anyway.
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors(modules, result, out);
                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
            HostRequest::ClearDisableLatch(modules) => {
                ringbuf_entry!(Trace::ClearDisableLatch(modules));
                let mask = LogicalPortMask::from(modules);
                self.disabled &= !mask;
                // This operation just sets internal SP state, so it is always
                // successful. However, invalid modules may been specified by
                // the host so we need to check that anyway.
                let result =
                    ModuleResultNoFailure::new(mask, LogicalPortMask(0))
                        .unwrap();
                let success = ModuleId::from(result.success());
                let (num_err_bytes, failed_modules) =
                    self.handle_errors(modules, result, out);
                (
                    MessageBody::SpResponse(SpResponse::Ack {
                        modules: success,
                        failed_modules,
                    }),
                    num_err_bytes,
                )
            }
        }
    }

    /// This function reads a `ModuleResult` and populates and failure or error
    /// information at the end of the trailing data buffer, taking
    /// `self.disabled` into account. This means it should be called as the last
    /// operation before sending the response. For results where a
    /// `ModuleResultNoFailure` is returned, use `handle_errors` or
    /// `handle_errors_and_disabled` instead.
    ///
    /// Panics: This function can panic if a module has been marked to have had
    /// a failure but the failure type enum is NoError. In practice this should
    /// not happen.
    fn handle_errors_and_failures_and_disabled(
        &mut self,
        modules: ModuleId,
        result: ModuleResult,
        out: &mut [u8],
    ) -> (usize, ModuleId) {
        let mut error_idx: usize = 0;
        // any modules at index 32->63 are not currently supported.
        let invalid_modules = ModuleId(0xffffffff00000000);
        let requested_invalid_modules = modules & invalid_modules;

        let disabled: ModuleId = self.disabled.into();
        let requested_disabled_modules = modules & disabled;

        for module in modules.to_indices().map(LogicalPort) {
            if module <= LogicalPortMask::MAX_PORT_INDEX {
                if requested_disabled_modules.is_set(module.0).unwrap() {
                    // let the host know it requested disabled modules
                    let err_size = hubpack::serialize(
                        &mut out[error_idx..],
                        &HwError::DisabledBySp,
                    )
                    .unwrap();
                    error_idx += err_size;
                } else if result.failure().is_set(module) {
                    let failure_type = match result.failure_types()[module] {
                        FpgaI2CFailure::NoModule => HwError::NotPresent,
                        FpgaI2CFailure::NoPower => HwError::NotPowered,
                        FpgaI2CFailure::PowerFault => HwError::PowerFault,
                        FpgaI2CFailure::NotInitialized => {
                            HwError::NotInitialized
                        }
                        FpgaI2CFailure::I2CAddressNack => {
                            HwError::I2cAddressNack
                        }
                        FpgaI2CFailure::I2CByteNack => HwError::I2cByteNack,
                        FpgaI2CFailure::I2CSclStretchTimeout => {
                            HwError::I2cSclStretchTimeout
                        }
                        // We only mark failures when an error is set, so this
                        // branch should never match.
                        FpgaI2CFailure::NoError => unreachable!(),
                    };
                    // failure: whatever `HwError` specified by `failure_type`
                    let err_size = hubpack::serialize(
                        &mut out[error_idx..],
                        &failure_type,
                    )
                    .unwrap();
                    error_idx += err_size;
                } else if result.error().is_set(module) {
                    let err_type = self.resolve_error_type();
                    let err_size =
                        hubpack::serialize(&mut out[error_idx..], &err_type)
                            .unwrap();
                    error_idx += err_size;
                }
            } else if requested_invalid_modules.is_set(module.0).unwrap() {
                // let the host know it requested unsupported modules
                let err_size = hubpack::serialize(
                    &mut out[error_idx..],
                    &HwError::InvalidModuleIndex,
                )
                .unwrap();
                error_idx += err_size;
            }
        }

        // let the caller know how many error bytes we appended and which
        // modules had problems
        (
            error_idx,
            ModuleId(
                requested_invalid_modules.0
                    | requested_disabled_modules.0
                    | result.failure().0 as u64
                    | result.error().0 as u64,
            ),
        )
    }

    /// This function reads a `ModuleResultNoFailure` and populates error
    /// information at the end of the trailing data buffer, ignoring
    /// `self.disabled`. This means it should be called as the last operation
    /// before sending the response. For results where a `ModuleResult` is
    /// returned, use `handle_errors_and_failures_and_disabled` instead.
    fn handle_errors(
        &mut self,
        modules: ModuleId,
        result: ModuleResultNoFailure,
        out: &mut [u8],
    ) -> (usize, ModuleId) {
        let mut error_idx: usize = 0;
        // any modules at index 32->63 are not currently supported.
        let invalid_modules = ModuleId(0xffffffff00000000);
        let requested_invalid_modules = modules & invalid_modules;
        for module in modules.to_indices().map(LogicalPort) {
            if module <= LogicalPortMask::MAX_PORT_INDEX
                && result.error().is_set(module)
            {
                let err_type = self.resolve_error_type();
                let err_size =
                    hubpack::serialize(&mut out[error_idx..], &err_type)
                        .unwrap();
                error_idx += err_size;
            } else if requested_invalid_modules.is_set(module.0).unwrap() {
                // let the host know it requested unsupported modules
                let err_size = hubpack::serialize(
                    &mut out[error_idx..],
                    &HwError::InvalidModuleIndex,
                )
                .unwrap();
                error_idx += err_size;
            }
        }

        // let the caller know how many error bytes we appended and which
        // modules had problems
        (
            error_idx,
            ModuleId(requested_invalid_modules.0 | result.error().0 as u64),
        )
    }

    // shared logic between the various functions which handle errors
    fn resolve_error_type(&self) -> HwError {
        match self.front_io.board_status() {
            // Front IO is present and ready, so the only other error
            // path currently is if we handle an FpgaError.
            FrontIOStatus::Ready => HwError::FpgaError,
            FrontIOStatus::NotPresent => HwError::NoFrontIo,
            FrontIOStatus::Init
            | FrontIOStatus::FpgaInit
            | FrontIOStatus::OscInit => HwError::FrontIoNotReady,
        }
    }

    /// This function reads a `ModuleResultNoFailure` and populates error
    /// information at the end of the trailing data buffer, taking `self.data`
    /// into account. This means it should be called as the last operation
    /// before sending the response. For results where a `ModuleResult` is
    /// returned, use `handle_errors_and_failures_and_disabled` instead.
    fn handle_errors_and_disabled(
        &mut self,
        modules: ModuleId,
        result: ModuleResultNoFailure,
        out: &mut [u8],
    ) -> (usize, ModuleId) {
        let mut error_idx: usize = 0;
        // any modules at index 32->63 are not currently supported.
        let invalid_modules = ModuleId(0xffffffff00000000);
        let requested_invalid_modules = modules & invalid_modules;

        // any modules that are listed in self.disabled are also not supported
        let disabled: ModuleId = self.disabled.into();
        let requested_disabled_modules = modules & disabled;

        for module in modules.to_indices().map(LogicalPort) {
            if module <= LogicalPortMask::MAX_PORT_INDEX {
                if requested_disabled_modules.is_set(module.0).unwrap() {
                    // let the host know it requested unsupported modules
                    let err_size = hubpack::serialize(
                        &mut out[error_idx..],
                        &HwError::DisabledBySp,
                    )
                    .unwrap();
                    error_idx += err_size;
                } else if result.error().is_set(module) {
                    // error: fpga communication issue
                    let err_size = hubpack::serialize(
                        &mut out[error_idx..],
                        &HwError::FpgaError,
                    )
                    .unwrap();
                    error_idx += err_size;
                }
            } else if requested_invalid_modules.is_set(module.0).unwrap() {
                // let the host know it requested unsupported modules
                let err_size = hubpack::serialize(
                    &mut out[error_idx..],
                    &HwError::InvalidModuleIndex,
                )
                .unwrap();
                error_idx += err_size;
            }
        }

        // let the caller know how many error bytes we appended and which
        // modules had problems
        (
            error_idx,
            ModuleId(
                requested_invalid_modules.0
                    | requested_disabled_modules.0
                    | result.error().0 as u64,
            ),
        )
    }

    fn get_status(
        &mut self,
        modules: LogicalPortMask,
        out: &mut [u8],
    ) -> (usize, ModuleResultNoFailure) {
        // This will get the status of every module, so we will have to only
        // select the data which was requested.
        let xcvr_status = self.front_io.transceivers_status();
        let mod_status = xcvr_status.status;
        let full_result = xcvr_status.result;
        // adjust the result success mask to be only our requested modules
        let desired_result = ModuleResultNoFailure::new(
            full_result.success() & modules,
            full_result.error() & modules,
        )
        .unwrap();

        // Write one bitfield per active port in the ModuleId which was
        // successfully retrieved above.
        let mut count = 0;
        for mask in modules
            .to_indices()
            .filter(|&p| desired_result.success().is_set(p))
            .map(|p| p.as_mask())
        {
            let mut status = Status::empty();
            if (mod_status.power_enable & mask.0) != 0 {
                status |= Status::ENABLED;
            }
            if (!mod_status.resetl & mask.0) != 0 {
                status |= Status::RESET;
            }
            if (mod_status.lpmode_txdis & mask.0) != 0 {
                status |= Status::LOW_POWER_MODE;
            }
            if (!mod_status.modprsl & mask.0) != 0 {
                status |= Status::PRESENT;
            }
            if (!mod_status.intl_rxlosl & mask.0) != 0 {
                status |= Status::INTERRUPT;
            }
            if (mod_status.power_good & mask.0) != 0 {
                status |= Status::POWER_GOOD;
            }
            if (mod_status.power_good_timeout & mask.0) != 0 {
                status |= Status::FAULT_POWER_TIMEOUT;
            }
            if (mod_status.power_good_fault & mask.0) != 0 {
                status |= Status::FAULT_POWER_LOST;
            }
            // Convert from Status -> u8 and write to the output buffer
            out[count] = status.bits();
            count += 1;
        }
        (count, desired_result)
    }

    fn get_extended_status(
        &mut self,
        modules: LogicalPortMask,
        out: &mut [u8],
    ) -> (usize, ModuleResultNoFailure) {
        // This will get the status of every module, so we will have to only
        // select the data which was requested.
        let xcvr_status = self.front_io.transceivers_status();
        let mod_status = xcvr_status.status;
        let full_result = xcvr_status.result;
        // adjust the result success mask to be only our requested modules
        let desired_result = ModuleResultNoFailure::new(
            full_result.success() & modules,
            full_result.error() & modules,
        )
        .unwrap();

        // Write one u32 bitfield per active port in the ModuleId which was
        // successfully retrieved above.
        let mut count = 0;
        for mask in modules
            .to_indices()
            .filter(|&p| desired_result.success().is_set(p))
            .map(|p| p.as_mask())
        {
            let mut status = ExtendedStatus::empty();
            if (mod_status.power_enable & mask.0) != 0 {
                status |= ExtendedStatus::ENABLED;
            }
            if (!mod_status.resetl & mask.0) != 0 {
                status |= ExtendedStatus::RESET;
            }
            if (mod_status.lpmode_txdis & mask.0) != 0 {
                status |= ExtendedStatus::LOW_POWER_MODE;
            }
            if (!mod_status.modprsl & mask.0) != 0 {
                status |= ExtendedStatus::PRESENT;
            }
            if (!mod_status.intl_rxlosl & mask.0) != 0 {
                status |= ExtendedStatus::INTERRUPT;
            }
            if (mod_status.power_good & mask.0) != 0 {
                status |= ExtendedStatus::POWER_GOOD;
            }
            if (mod_status.power_good_timeout & mask.0) != 0 {
                status |= ExtendedStatus::FAULT_POWER_TIMEOUT;
            }
            if (mod_status.power_good_fault & mask.0) != 0 {
                status |= ExtendedStatus::FAULT_POWER_LOST;
            }
            if !(self.disabled & mask).is_empty() {
                status |= ExtendedStatus::DISABLED_BY_SP;
            }
            count +=
                hubpack::serialize(&mut out[count..], &status.bits()).unwrap();
        }
        (count, desired_result)
    }

    fn select_page(
        &mut self,
        page: Page,
        mask: LogicalPortMask,
    ) -> ModuleResult {
        // Common to both CMIS and SFF-8636
        const BANK_SELECT: u8 = 0x7E;
        const PAGE_SELECT: u8 = 0x7F;

        let mut result = ModuleResult::new(
            mask,
            LogicalPortMask(0),
            LogicalPortMask(0),
            LogicalPortFailureTypes::default(),
        )
        .unwrap_lite();

        // We can always write the lower page; upper pages require modifying
        // registers in the transceiver to select it.
        if let Some(page) = page.page() {
            result = match self
                .front_io
                .transceivers_set_i2c_write_buffer(mask, &[page])
            {
                Ok(r) => result.chain(r),
                Err(e) => {
                    ringbuf_entry!(Trace::FrontIOError(e));
                    result
                }
            };

            result = match self.front_io.transceivers_setup_i2c_write(
                PAGE_SELECT,
                1,
                mask,
            ) {
                Ok(r) => result.chain(r),
                Err(e) => {
                    ringbuf_entry!(Trace::FrontIOError(e));
                    result
                }
            };

            result = result.chain(self.wait_and_check_i2c(result.success()));
        } else {
            // If the request is to the lower page it is always successful
            result = ModuleResult::new(
                mask,
                LogicalPortMask(0),
                LogicalPortMask(0),
                LogicalPortFailureTypes::default(),
            )
            .unwrap_lite();
        }

        if let Some(bank) = page.bank() {
            result = match self
                .front_io
                .transceivers_set_i2c_write_buffer(mask, &[bank])
            {
                Ok(r) => result.chain(r),
                Err(e) => {
                    ringbuf_entry!(Trace::FrontIOError(e));
                    result
                }
            };

            result = match self.front_io.transceivers_setup_i2c_write(
                BANK_SELECT,
                1,
                result.success(),
            ) {
                Ok(r) => result.chain(r),
                Err(e) => {
                    ringbuf_entry!(Trace::FrontIOError(e));
                    result
                }
            };

            result = result.chain(self.wait_and_check_i2c(result.success()));
        }

        for port in result.failure().to_indices() {
            ringbuf_entry!(Trace::PageSelectI2CFailures(
                port,
                result.failure_types()[port]
            ))
        }
        result
    }

    // Polls the status register for each module in the mask. The returned
    // ModuleResult is of the form:
    // success: The I2C operation completed successfully.
    // failure: The I2C operation failed.
    // error: The SP could not communicate with the FPGA.
    fn wait_and_check_i2c(&mut self, mask: LogicalPortMask) -> ModuleResult {
        self.front_io.transceivers_wait_and_check_i2c(mask)
    }

    fn read(
        &mut self,
        mem: MemoryRead,
        modules: LogicalPortMask,
        out: &mut [u8],
    ) -> ModuleResult {
        // Switch pages (if necessary)
        let mut result = self.select_page(*mem.page(), modules);

        // Ask the FPGA to start the read
        result = match self.front_io.transceivers_setup_i2c_read(
            mem.offset(),
            mem.len(),
            result.success(),
        ) {
            Ok(r) => result.chain(r),
            Err(e) => {
                ringbuf_entry!(Trace::FrontIOError(e));
                result
            }
        };

        let mut success = LogicalPortMask(0);
        let mut failure = LogicalPortMask(0);
        let mut error = LogicalPortMask(0);
        let mut idx = 0;
        let buf_len = mem.len() as usize;
        let mut failure_types = LogicalPortFailureTypes::default();

        for port in result.success().to_indices() {
            let mut buf = [0u8; 128];

            // If we have not encountered any errors, keep pulling full
            // status + buffer payloads.
            let status = match self
                .front_io
                .transceivers_get_i2c_status_and_read_buffer(
                    port,
                    &mut buf[0..buf_len],
                ) {
                Ok(status) => status,
                Err(_) => {
                    error.set(port);
                    break;
                }
            };

            // Check error mask
            if status.error != FpgaI2CFailure::NoError {
                // Record which port the error ocurred at so we can
                // give the host a more meaningful error.
                failure.set(port);
                failure_types.0[port.0 as usize] = status.error
            } else {
                // Add data to payload
                success.set(port);
                let end_idx = idx + buf_len;
                out[idx..end_idx].copy_from_slice(&buf[..buf_len]);
                idx = end_idx;
            }
        }
        let mut final_result =
            ModuleResult::new(success, failure, error, failure_types)
                .unwrap_lite();
        final_result = result.chain(final_result);

        for port in final_result.failure().to_indices() {
            ringbuf_entry!(Trace::ReadI2CFailures(
                port,
                final_result.failure_types()[port]
            ))
        }
        final_result
    }

    // The `LogicalPortMask` indicates which of the requested ports the
    // `HwError` applies to.
    fn write(
        &mut self,
        mem: MemoryWrite,
        modules: LogicalPortMask,
        data: &[u8],
    ) -> ModuleResult {
        let mut result = self.select_page(*mem.page(), modules);

        // Copy data into the FPGA write buffer
        result = match self.front_io.transceivers_set_i2c_write_buffer(
            modules,
            &data[..mem.len() as usize],
        ) {
            Ok(r) => result.chain(r),
            Err(e) => {
                ringbuf_entry!(Trace::FrontIOError(e));
                result
            }
        };

        // Trigger a multicast write to all transceivers in the mask
        result = match self.front_io.transceivers_setup_i2c_write(
            mem.offset(),
            mem.len(),
            result.success(),
        ) {
            Ok(r) => result.chain(r),
            Err(e) => {
                ringbuf_entry!(Trace::FrontIOError(e));
                result
            }
        };
        result = result.chain(self.wait_and_check_i2c(result.success()));

        for port in result.failure().to_indices() {
            ringbuf_entry!(Trace::WriteI2CFailures(
                port,
                result.failure_types()[port]
            ))
        }
        result
    }

    fn get_led_state_response(
        &mut self,
        modules: LogicalPortMask,
        out: &mut [u8],
    ) -> (usize, ModuleResultNoFailure) {
        let mut led_state_len = 0;
        for led_state in
            modules.to_indices().map(|m| self.front_io.led_get_state(m))
        {
            // LedState will serialize to a u8, so we aren't concerned about
            // buffer overflow here
            let led_state_size =
                hubpack::serialize(&mut out[led_state_len..], &led_state)
                    .unwrap();
            led_state_len += led_state_size;
        }

        (
            led_state_len,
            ModuleResultNoFailure::new(modules, LogicalPortMask(0)).unwrap(),
        )
    }
}
