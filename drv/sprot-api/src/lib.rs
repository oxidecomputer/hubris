// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.
//!
//! An SP/RoT SPI message is:
//!   1. A hubpack encoded `MsgHeader` containing the protocol, payload length,
//!      and message type.
//!   2. Payload according to MessageType, typically hubpack encoded
//!      structure(s) and/or bulk data.
//!   3. A CRC16 parameters that covers all of the bytes from the protocol ID to
//!      the end of the payload.
//!

#![no_std]
extern crate memoffset;

use crc::{Crc, CRC_16_XMODEM};
use derive_idol_err::IdolError;
use drv_update_api::{ImageVersion, UpdateError, UpdateTarget};
use hubpack::SerializedSize;
use if_chain::if_chain; // Chained if let statements are almost here.
use serde::{Deserialize, Serialize};
// use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};
use zerocopy::{AsBytes, FromBytes};

/// The canonical SpRot protocol error returned by this API
//
// TODO: Audit that each MsgError is used and has some reasonable action.
// While a diverse set of error codes may be useful for debugging it
// clutters code that just has to deal with the error.
// then consider adding a function that translates an error code
// into the desired action, e.g. InvalidCrc and FlowError should both
// result in a retry on the SP side and an ErrorRsp on the RoT side.
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    IdolError,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub enum SprotError {
    /// There is no message
    NoMessage = 1,
    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadTransferSize = 2,
    /// Server restarted
    // ServerRestarted = 3,
    /// CRC check failed.
    InvalidCrc = 4,
    /// FIFO overflow/underflow
    FlowError = 5,
    /// Unsupported protocol version
    UnsupportedProtocol = 6,
    /// Unknown message
    BadMessageType = 7,
    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadMessageLength = 8,
    /// Error from Spi
    SpiServerError = 9,
    /// Message is too large
    Oversize = 10,
    /// Tx buffer is unexpectedly not Idle.
    TxNotIdle = 11,

    CannotAssertCSn = 12,
    RotNotReady = 13,
    RspTimeout = 14,
    BadResponse = 15,
    RotBusy = 16,
    /// Feature is not implemented
    NotImplemented = 17,
    /// An error code reserved for the SP was used by the Rot
    NonRotError = 18,
    /// A message with version = 0 was received unexpectedly.
    EmptyMessage = 19,

    /// Insufficient bytes received
    Incomplete = 20,
    /// Hubpack error
    Serialization = 21,
    /// Sequence number mismatch in Sink test
    Sequence = 22,

    //
    // Update Related Errors
    //
    UpdateBadLength = 23,
    UpdateInProgress = 24,
    UpdateOutOfBounds = 25,
    UpdateTimeout = 26,
    UpdateEccDoubleErr = 27,
    UpdateEccSingleErr = 28,
    UpdateSecureErr = 29,
    UpdateReadProtErr = 30,
    UpdateWriteEraseErr = 31,
    UpdateInconsistencyErr = 32,
    UpdateStrobeErr = 33,
    UpdateProgSeqErr = 34,
    UpdateWriteProtErr = 35,
    UpdateBadImageType = 36,
    UpdateAlreadyFinished = 37,
    UpdateNotStarted = 38,
    UpdateRunningImage = 39,
    UpdateFlashError = 40,
    UpdateSpRotError = 41,
    UpdateUnknown = 42,

    /// Unknown Errors are mapped to 0xff
    Unknown = 0xff,
}

impl From<UpdateError> for SprotError {
    fn from(value: UpdateError) -> Self {
        match value {
            UpdateError::BadLength => SprotError::UpdateBadLength,
            UpdateError::UpdateInProgress => SprotError::UpdateInProgress,
            UpdateError::OutOfBounds => SprotError::UpdateOutOfBounds,
            UpdateError::Timeout => SprotError::UpdateTimeout,
            UpdateError::EccDoubleErr => SprotError::UpdateEccDoubleErr,
            UpdateError::EccSingleErr => SprotError::UpdateEccSingleErr,
            UpdateError::SecureErr => SprotError::UpdateSecureErr,
            UpdateError::ReadProtErr => SprotError::UpdateReadProtErr,
            UpdateError::WriteEraseErr => SprotError::UpdateWriteEraseErr,
            UpdateError::InconsistencyErr => SprotError::UpdateInconsistencyErr,
            UpdateError::StrobeErr => SprotError::UpdateStrobeErr,
            UpdateError::ProgSeqErr => SprotError::UpdateProgSeqErr,
            UpdateError::WriteProtErr => SprotError::UpdateWriteProtErr,
            UpdateError::BadImageType => SprotError::UpdateBadImageType,
            UpdateError::UpdateAlreadyFinished => {
                SprotError::UpdateAlreadyFinished
            }
            UpdateError::UpdateNotStarted => SprotError::UpdateNotStarted,
            UpdateError::RunningImage => SprotError::UpdateRunningImage,
            UpdateError::FlashError => SprotError::UpdateFlashError,
            UpdateError::SpRotError => SprotError::UpdateSpRotError,
            UpdateError::Unknown => SprotError::UpdateUnknown,
        }
    }
}

impl From<u8> for SprotError {
    fn from(byte: u8) -> SprotError {
        match byte {
            1 => SprotError::NoMessage,
            2 => SprotError::BadTransferSize,
            4 => SprotError::InvalidCrc,
            5 => SprotError::FlowError,
            6 => SprotError::UnsupportedProtocol,
            7 => SprotError::BadMessageType,
            8 => SprotError::BadMessageLength,
            9 => SprotError::SpiServerError,
            10 => SprotError::Oversize,
            11 => SprotError::TxNotIdle,
            12 => SprotError::CannotAssertCSn,
            13 => SprotError::RotNotReady,
            14 => SprotError::RspTimeout,
            15 => SprotError::BadResponse,
            16 => SprotError::RotBusy,
            17 => SprotError::NotImplemented,
            18 => SprotError::NonRotError,
            19 => SprotError::EmptyMessage,
            20 => SprotError::Incomplete,
            21 => SprotError::Serialization,
            22 => SprotError::Sequence,
            23 => SprotError::UpdateBadLength,
            24 => SprotError::UpdateInProgress,
            25 => SprotError::UpdateOutOfBounds,
            26 => SprotError::UpdateTimeout,
            27 => SprotError::UpdateEccDoubleErr,
            28 => SprotError::UpdateEccSingleErr,
            29 => SprotError::UpdateSecureErr,
            30 => SprotError::UpdateReadProtErr,
            31 => SprotError::UpdateWriteEraseErr,
            32 => SprotError::UpdateInconsistencyErr,
            33 => SprotError::UpdateStrobeErr,
            34 => SprotError::UpdateProgSeqErr,
            35 => SprotError::UpdateWriteProtErr,
            36 => SprotError::UpdateBadImageType,
            37 => SprotError::UpdateAlreadyFinished,
            38 => SprotError::UpdateNotStarted,
            39 => SprotError::UpdateRunningImage,
            40 => SprotError::UpdateFlashError,
            41 => SprotError::UpdateSpRotError,
            42 => SprotError::UpdateUnknown,
            _ => SprotError::Unknown,
        }
    }
}

impl From<hubpack::Error> for SprotError {
    fn from(_: hubpack::Error) -> Self {
        SprotError::Serialization
    }
}

/// The successful result of pulsing the active low chip-select line
#[derive(
    Copy, Clone, FromBytes, AsBytes, Serialize, Deserialize, SerializedSize,
)]
#[repr(C, packed)]
pub struct PulseStatus {
    pub rot_irq_begin: u8,
    pub rot_irq_end: u8,
}

/// The result of a bulk sink transfer test
#[derive(
    Copy, Clone, FromBytes, AsBytes, Serialize, Deserialize, SerializedSize,
)]
#[repr(C, packed)]
pub struct SinkStatus {
    pub sent: u16,
}

/// SP/RoT interface configuration and status.
///
/// This is meant to be a forward compatible, insecure, informational
/// structure used to facilitate manufacturing workflows and diagnosis
/// of problems before trusted communications can be established.
///
/// All of the counters will wrap around.
///
/// TODO: Finalize this structure before first customer ship.
#[derive(
    Copy, Clone, FromBytes, AsBytes, Serialize, Deserialize, SerializedSize,
)]
#[repr(C, packed)]
pub struct Status {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,

    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    pub bootrom_crc32: u32,

    /// Firmware epoch (defines update window)
    pub epoch: u32,

    /// The currently running firmware version.
    pub version: u32,

    /// Maxiumum message size that the RoT can handle.
    pub buffer_size: u32,

    /// Number of messages received
    pub rx_received: u32,

    /// Number of messages where the RoT failed to service the Rx FIFO in time.
    pub rx_overrun: u32,

    /// Number of messages where the RoT failed to service the Tx FIFO in time.
    pub tx_underrun: u32,

    /// Number of invalid messages received
    pub rx_invalid: u32,

    /// Number of incomplete transmissions (valid data not fetched by SP).
    pub tx_incomplete: u32,
}

#[derive(
    Copy, Clone, FromBytes, AsBytes, Serialize, Deserialize, SerializedSize,
)]
#[repr(C, packed)]
pub struct Received {
    pub length: u16,
    pub msgtype: u8,
}

/// Protocol version
#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    FromPrimitive,
    Deserialize,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub enum Protocol {
    /// Indicates that no message is present.
    Ignore = 0x00,
    /// The only supported message format at this time.
    V1 = 0x01,
    /// Never to be seen. Queued by RoT when not ready.
    Busy = 0xb2,
    Unsupported = 0xff,
}

impl From<u8> for Protocol {
    fn from(value: u8) -> Protocol {
        match value {
            0x00 => Protocol::Ignore,
            0x01 => Protocol::V1,
            0xb2 => Protocol::Busy,
            _ => Protocol::Unsupported,
        }
    }
}

// SPI has no flow control, i.e. the RoT has no busy state indication
// visible to the SP.
// In future designs, we could consider adding a "ROT_READY" in addition
// to ROT_IRQ or use an interconect that does have flow control.
//
// The RoT places 0xB2 (Busy -> "B" "Z" -> 0xB2) in its transmit FIFO when
// it is not prepared to service SPI IO. If the SP ever clocks out
// data before the RoT is ready, the SP will read 0xB2.
// This code should never be seen on the SPI bus. If seen as the first
// byte in a message or on the SPI bus with a logic analyzer, that needs
// to be investigated and fixed.
//
// It may mean that the two parties are out of phase, some RoT task
// is hampering the sprot task from meeting its realtime requirements,
// or there is some other bug.
// pub const VERSION_BUSY: u8 = 0xB2;

/// SPI Message types will allow for multiplexing and forward compatibility.
#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    FromPrimitive,
    AsBytes,
    Deserialize,
    Serialize,
    SerializedSize,
)]
#[repr(u8)]
pub enum MsgType {
    /// A reserved value.
    Invalid = 0,
    /// A response to a message that was not valid.
    ErrorRsp = 1,
    /// Request that the RoT send back the message payload in an EchoRsp
    EchoReq = 2,
    /// The response to an EchoReq message
    EchoRsp = 3,
    /// Request RoT status.
    StatusReq = 4,
    /// Supply RoT status.
    // TODO: decide on appropriate content for the StatusRsp message payload.
    StatusRsp = 5,
    /// Payload contains a sprockets request.
    SprocketsReq = 6,
    /// Payload contains a sprockets response.
    SprocketsRsp = 7,
    /// RoT sinks this message sending back a SinkReq with no payload.
    SinkReq = 8,
    /// Acknowledge recepit of SinkReq
    SinkRsp = 9,

    /// Update API (see udate.idol)
    UpdBlockSizeReq = 10,
    UpdBlockSizeRsp = 11,
    UpdPrepImageUpdateReq = 12,
    UpdPrepImageUpdateRsp = 13,
    UpdWriteOneBlockReq = 14,
    UpdWriteOneBlockRsp = 15,
    UpdAbortUpdateReq = 16,
    UpdAbortUpdateRsp = 17,
    UpdFinishImageUpdateReq = 18,
    UpdFinishImageUpdateRsp = 19,
    UpdCurrentVersionReq = 20,
    UpdCurrentVersionRsp = 21,

    /// Reserved value.
    Unknown = 0xff,
}

impl From<u8> for MsgType {
    fn from(value: u8) -> MsgType {
        match value {
            0 => MsgType::Invalid,
            1 => MsgType::ErrorRsp,
            2 => MsgType::EchoReq,
            3 => MsgType::EchoRsp,
            4 => MsgType::StatusReq,
            5 => MsgType::StatusRsp,
            6 => MsgType::SprocketsReq,
            7 => MsgType::SprocketsRsp,
            8 => MsgType::SinkReq,
            9 => MsgType::SinkRsp,
            10 => MsgType::UpdBlockSizeReq,
            11 => MsgType::UpdBlockSizeRsp,
            12 => MsgType::UpdPrepImageUpdateReq,
            13 => MsgType::UpdPrepImageUpdateRsp,
            14 => MsgType::UpdWriteOneBlockReq,
            15 => MsgType::UpdWriteOneBlockRsp,
            16 => MsgType::UpdAbortUpdateReq,
            17 => MsgType::UpdAbortUpdateRsp,
            18 => MsgType::UpdFinishImageUpdateReq,
            19 => MsgType::UpdFinishImageUpdateRsp,
            20 => MsgType::UpdCurrentVersionReq,
            21 => MsgType::UpdCurrentVersionRsp,
            _ => MsgType::Unknown,
        }
    }
}

// The SpRot Header prepended to each message traversing the SPI bus
// between the RoT and SP.
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
struct MsgHeader {
    protocol: Protocol,
    msgtype: MsgType,
    len: u16,
}

impl MsgHeader {
    /// Create a new `MsgHeader` with protocol version `V1`.
    ///
    /// Return an error if the message payload does not fit within a u16 or
    /// it is greater than `PAYLOAD_SIZE_MAX`.
    pub fn new_v1(
        msgtype: MsgType,
        payload_size: usize,
    ) -> Result<MsgHeader, SprotError> {
        if payload_size > PAYLOAD_SIZE_MAX {
            return Err(SprotError::BadMessageLength);
        }
        let len = u16::try_from(payload_size)
            .map_err(|_| SprotError::BadMessageLength)?;
        Ok(MsgHeader {
            protocol: Protocol::V1,
            msgtype,
            len,
        })
    }

    /// Serialize a `MsgHeader` into `buf`, returning the number of bytes written
    /// or an error if serialization fails.
    pub fn serialize(&self, buf: &mut [u8]) -> Result<usize, SprotError> {
        let size = hubpack::serialize(&mut buf[..HEADER_SIZE], self)?;
        Ok(size)
    }
}

/// A wrapper around a mutable buffer large enough to store (in order):
///   * A serialized `MsgHeader`
///   * A serialized message payload
///   * A 16 bit CRC
///
/// This is used for serializing data
pub struct MutMsgBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> MutMsgBuffer<'a> {
    pub fn new(buf: &'a mut [u8]) -> MutMsgBuffer<'a> {
        assert!(buf.len() >= BUF_SIZE);
        MutMsgBuffer { buf }
    }

    /// Serialize a V1 message. As long as `payload_size <= PAYLOAD_SIZE_MAX`, then
    /// serialization is guaranteed to succeed.
    pub fn serialize_v1(
        &mut self,
        msgtype: MsgType,
        payload_size: usize,
    ) -> Result<usize, SprotError> {
        let _ =
            MsgHeader::new_v1(msgtype, payload_size)?.serialize(self.buf)?;

        // At this point we know that the buffer is sized correctly and we can
        // unwrap. Otherwise the header would not have serialized successfully.
        let msg_bytes = self.buf.get(0..HEADER_SIZE + payload_size).unwrap();
        let crc = CRC16.checksum(msg_bytes);
        let crc_begin = HEADER_SIZE + payload_size;
        let end = crc_begin + CRC_SIZE;
        let crc_buf = self.buf.get_mut(crc_begin..end).unwrap();
        hubpack::serialize(crc_buf, &crc)?;
        Ok(end)
    }
}

/// A wrapper around an immutable buffer large enough to store (in order):
///   * A serialized `MsgHeader`
///   * A serialized message payload
///   * A 16 bit CRC
///
/// This is used for deserializing data
pub struct MsgBuffer<'a> {
    buf: &'a [u8],
}

impl<'a> MsgBuffer<'a> {
    pub fn new(buf: &'a [u8]) -> MsgBuffer<'a> {
        assert!(buf.len() >= BUF_SIZE);
        MsgBuffer { buf }
    }

    /// Deserialize a message, `M`, from the wrapped buffer
    //
    // TODO(AJS): There should be a general desserialize method
    // that also checks the header and does all the stuff included in `parse`.
    // That will simplify a lot.
    // Or we could provide a deserialize_header method that does all that.
    //
    pub fn deserialize_payload<M>(
        &self,
        payload_size: usize,
    ) -> Result<M, SprotError>
    where
        M: for<'de> Deserialize<'de>,
    {
        let end = HEADER_SIZE + payload_size;
        let (msg, _) = hubpack::deserialize::<M>(&self.buf[HEADER_SIZE..end])?;
        Ok(msg)
    }
}

pub type UpdateRspHeader = Result<Option<u32>, u32>;

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
pub const HEADER_SIZE: usize = <MsgHeader as SerializedSize>::MAX_SIZE;
const PAYLOAD_CMD_SIZE: usize = 64; // Allow for struct accompanying block data.
const PAYLOAD_BLOCK_SIZE: usize = 1024; // Allow for bulk data (arbitrary).
const PAYLOAD_SIZE_MAX: usize = PAYLOAD_CMD_SIZE + PAYLOAD_BLOCK_SIZE;
const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
pub const BUF_SIZE: usize = HEADER_SIZE + PAYLOAD_SIZE_MAX + CRC_SIZE;
pub const MIN_MSG_SIZE: usize = HEADER_SIZE + CRC_SIZE;
// XXX ROT FIFO size should be discovered.
pub const ROT_FIFO_SIZE: usize = 8;

// RX parse
//   given a buffer
//   - check protocol
//   - get length
//   - check length against buffer limits including CRC
//   - calculate CRC
//   - check CRC against stored CRC
//   - Result<(MsgType, &[u8]), MsgError>
pub fn parse(buffer: &[u8]) -> Result<(MsgType, &[u8]), SprotError> {
    if buffer.is_empty() {
        return Err(SprotError::NoMessage);
    }
    match Protocol::from(buffer[0]) {
        Protocol::Ignore => return Err(SprotError::NoMessage),
        Protocol::Busy => return Err(SprotError::RotBusy),
        Protocol::V1 => {}
        Protocol::Unsupported => return Err(SprotError::UnsupportedProtocol),
    }
    if buffer.len() < HEADER_SIZE + CRC_SIZE {
        return Err(SprotError::BadMessageLength);
    }
    if let Ok((header, payload_start)) =
        hubpack::deserialize::<MsgHeader>(buffer)
    {
        let len = header.len as usize;
        if_chain! {
            if let Some(crc_buf) = payload_start.get(len..len + CRC_SIZE);
            if let Ok((crc, _)) = hubpack::deserialize::<u16>(crc_buf);
            if let Some(msg_bytes) = buffer.get(0..HEADER_SIZE+len);
            if crc == CRC16.checksum(msg_bytes);
            then {
                Ok((header.msgtype, &payload_start[..len]))
            } else {
                Err(SprotError::InvalidCrc)
            }
        }
    } else {
        // Content didn't matter for hubpack::deserialize.
        // So, there weren't enough bytes to decode a header.
        Err(SprotError::BadMessageLength)
    }
}

/// Parse the header from an incomplete received message
pub fn rx_payload_remaining_mut(
    valid_bytes: usize,
    buffer: &mut [u8],
) -> Result<&mut [u8], SprotError> {
    if valid_bytes.min(buffer.len()) < HEADER_SIZE {
        return Err(SprotError::Incomplete);
    }
    match Protocol::from(buffer[0]) {
        Protocol::Ignore => return Err(SprotError::NoMessage),
        Protocol::Busy => return Err(SprotError::RotBusy),
        Protocol::V1 => (),
        _ => return Err(SprotError::UnsupportedProtocol),
    }
    if let Ok((header, _payload_start)) =
        hubpack::deserialize::<MsgHeader>(buffer)
    {
        let end = MIN_MSG_SIZE + (header.len as usize);
        if end > buffer.len() {
            Err(SprotError::BadMessageLength)
        } else if valid_bytes < end {
            Ok(&mut buffer[valid_bytes..end])
        } else {
            Ok(&mut buffer[end..end])
        }
    } else {
        Err(SprotError::Serialization)
    }
}

/// Read access to the first portion or all of the transmit buffer.
pub fn payload_buf(size: Option<usize>, buffer: &[u8]) -> &[u8] {
    let start = HEADER_SIZE;
    let end = match size {
        Some(size) => HEADER_SIZE + size,
        None => buffer.len() - CRC_SIZE,
    };
    match buffer.get(start..end) {
        Some(buf) => buf,
        None => panic!(), // Don't come to me with your miniscule buffers.
    }
}

/// Read/write access to the first portion or all of the transmit buffer.
pub fn payload_buf_mut(size: Option<usize>, buffer: &mut [u8]) -> &mut [u8] {
    let start = HEADER_SIZE;
    let end = match size {
        Some(size) => HEADER_SIZE + size,
        None => buffer.len() - CRC_SIZE,
    };
    match buffer.get_mut(start..end) {
        Some(buf) => buf,
        None => panic!(), // Don't come to me with your miniscule buffers.
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
