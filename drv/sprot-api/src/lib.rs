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
//!   3. A CRC16 value that covers all of the bytes from the protocol ID to
//!      the end of the payload.
//!

#![no_std]
extern crate memoffset;

use crc::{Crc, CRC_16_XMODEM};
use derive_idol_err::IdolError;
use drv_update_api::{
    HandoffDataLoadError, RotBootState, UpdateError, UpdateTarget,
};
use hubpack::SerializedSize;
use idol_runtime::{Leased, R};
use serde::{Deserialize, Serialize};
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
    /// A task crashed during an operation, commonly a lease read or write
    TaskRestart = 3,
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

    // An error relating to Stage0 handoff of image data
    Stage0HandoffError = 43,

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
        Self::from_u8(byte).unwrap_or(SprotError::Unknown)
    }
}

impl From<hubpack::Error> for SprotError {
    fn from(_: hubpack::Error) -> Self {
        SprotError::Serialization
    }
}

impl From<HandoffDataLoadError> for SprotError {
    fn from(_: HandoffDataLoadError) -> Self {
        SprotError::Stage0HandoffError
    }
}

// Return true if the error is recoverable, otherwise return false
pub fn is_recoverable_error(err: SprotError) -> bool {
    matches!(
        err,
        SprotError::InvalidCrc
            | SprotError::EmptyMessage
            | SprotError::RotNotReady
            | SprotError::RotBusy
    )
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
/// TODO: Finalize this structure before first customer ship.
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct SprotStatus {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,

    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    /// TODO: This should live in the stage0 handoff info
    pub bootrom_crc32: u32,

    /// Maxiumum message size that the RoT can handle.
    pub buffer_size: u32,

    pub rot_updates: RotBootState,
}

/// Stats from the RoT side of sprot
///
/// All of the counters will wrap around.
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub struct IoStats {
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

    // Rot/Spi related metrics useful for debugging
    IoStatsReq = 20,
    IoStatsRsp = 21,

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
            20 => MsgType::IoStatsReq,
            21 => MsgType::IoStatsRsp,
            _ => MsgType::Unknown,
        }
    }
}

/// A builder/serializer for messages that wraps the transmit buffer
///
/// Each public method returns the serialized buffer that can be sent on the
/// wire.
pub struct TxMsg {
    buf: [u8; BUF_SIZE],
}

impl AsMut<[u8]> for TxMsg {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..]
    }
}

impl Default for TxMsg {
    fn default() -> Self {
        Self::new()
    }
}

impl TxMsg {
    pub fn new() -> TxMsg {
        TxMsg { buf: [0; BUF_SIZE] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..]
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.buf[HEADER_SIZE..BUF_SIZE - CRC_SIZE]
    }

    /// Serialize an ErrorRsp with a one byte payload, which is a serialized
    /// `SprotError`
    pub fn error_rsp(&mut self, err: SprotError) -> VerifiedTxMsg {
        let payload_size = 1;
        self.buf[HEADER_SIZE] = err as u8;
        self.from_existing(MsgType::ErrorRsp, payload_size)
            .unwrap_lite()
    }

    /// Serialize a request with no payload
    pub fn no_payload(&mut self, msgtype: MsgType) -> VerifiedTxMsg {
        let payload_size = 0;
        self.write_header(msgtype, payload_size);
        self.write_crc(payload_size)
    }

    /// Serialize a request from a MsgType and Lease
    pub fn from_lease(
        &mut self,
        msgtype: MsgType,
        source: Leased<R, [u8]>,
    ) -> Result<VerifiedTxMsg, SprotError> {
        if source.len() > PAYLOAD_SIZE_MAX {
            return Err(SprotError::Oversize);
        }

        let dest = &mut self.buf[HEADER_SIZE..][..source.len()];
        source
            .read_range(0..source.len(), dest)
            .map_err(|_| SprotError::TaskRestart)?;

        self.write_header(msgtype, source.len());
        Ok(self.write_crc(source.len()))
    }

    /// Serialize a request into `self.buf` with an already written payload
    /// inside `self.buf`.
    pub fn from_existing(
        &mut self,
        msgtype: MsgType,
        payload_size: usize,
    ) -> Result<VerifiedTxMsg, SprotError> {
        if payload_size > PAYLOAD_SIZE_MAX {
            return Err(SprotError::Oversize);
        }
        self.write_header(msgtype, payload_size);
        Ok(self.write_crc(payload_size))
    }

    fn write_header(&mut self, msgtype: MsgType, payload_size: usize) {
        let _ = MsgHeader::new_v1(msgtype, payload_size)
            .unwrap_lite()
            .serialize(&mut self.buf[..])
            .unwrap_lite();
    }

    fn write_crc(&mut self, payload_size: usize) -> VerifiedTxMsg {
        let crc_begin = HEADER_SIZE + payload_size;
        let msg_bytes = &self.buf[0..crc_begin];
        let crc = CRC16.checksum(msg_bytes);
        let end = crc_begin + CRC_SIZE;
        let crc_buf = &mut self.buf[crc_begin..end];
        let _ = hubpack::serialize(crc_buf, &crc).unwrap_lite();
        VerifiedTxMsg(end)
    }
}

/// A type indicating that a complete message has been successfully serialized and therefore
/// fits in the allocated buffer.
#[derive(Clone, Copy)]
pub struct VerifiedTxMsg(pub usize);

/// A parser/deserializer for messages received over SPI
pub struct RxMsg {
    buf: [u8; BUF_SIZE],
}

impl AsMut<[u8]> for RxMsg {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.buf[..]
    }
}

impl Default for RxMsg {
    fn default() -> Self {
        Self::new()
    }
}

impl RxMsg {
    pub fn new() -> RxMsg {
        RxMsg { buf: [0; BUF_SIZE] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..]
    }

    pub fn payload(&self, rxmsg: &VerifiedRxMsg) -> &[u8] {
        &self.buf[HEADER_SIZE..][..rxmsg.0.payload_len as usize]
    }

    pub fn parse_header(
        &self,
        valid_bytes: usize,
    ) -> Result<MsgHeader, SprotError> {
        // We want to be able to return `RotBusy` before `Incomplete`.
        self.parse_protocol()?;
        if valid_bytes < HEADER_SIZE {
            return Err(SprotError::Incomplete);
        }
        let (header, _) = hubpack::deserialize::<MsgHeader>(&self.buf[..])?;
        if header.payload_len as usize > PAYLOAD_SIZE_MAX {
            return Err(SprotError::BadMessageLength);
        }
        Ok(header)
    }

    pub fn validate_crc(&self, header: &MsgHeader) -> Result<(), SprotError> {
        // The only way to get a `MsgHeader` is to call parse_header, which
        // already ensured that the payload size fits in the buffer.
        let crc_start = HEADER_SIZE + (header.payload_len as usize);
        let crc_buf = &self.buf[crc_start..][..CRC_SIZE];
        let (expected, _) = hubpack::deserialize::<u16>(crc_buf)?;
        let actual = CRC16.checksum(&self.buf[..crc_start]);
        if actual == expected {
            Ok(())
        } else {
            Err(SprotError::InvalidCrc)
        }
    }

    /// Deserialize a hubpack encoded message, `M`, from the wrapped buffer
    pub fn deserialize_hubpack_payload<M>(
        &self,
        rxmsg: &VerifiedRxMsg,
    ) -> Result<M, SprotError>
    where
        M: for<'de> Deserialize<'de>,
    {
        let (msg, _) = hubpack::deserialize::<M>(self.payload(rxmsg))?;
        Ok(msg)
    }

    /// Parse the first byte of the protocol, returning an appropriate error
    /// if necessary.
    fn parse_protocol(&self) -> Result<(), SprotError> {
        match Protocol::from(self.buf[0]) {
            Protocol::Ignore => Err(SprotError::NoMessage),
            Protocol::Busy => Err(SprotError::RotBusy),
            Protocol::V1 => Ok(()),
            _ => Err(SprotError::UnsupportedProtocol),
        }
    }
}

/// A type indicating that the message header has been parsed and the CRC has
/// been successfully verified
pub struct VerifiedRxMsg(pub MsgHeader);

// The SpRot Header prepended to each message traversing the SPI bus
// between the RoT and SP.
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub struct MsgHeader {
    pub protocol: Protocol,
    pub msgtype: MsgType,
    // Length of the payload (does not include Header or CRC)
    pub payload_len: u16,
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
            payload_len: len,
        })
    }

    /// Serialize a `MsgHeader` into `buf`, returning the number of bytes written
    /// or an error if serialization fails.
    pub fn serialize(&self, buf: &mut [u8]) -> Result<usize, SprotError> {
        let size = hubpack::serialize(&mut buf[..HEADER_SIZE], self)?;
        Ok(size)
    }
}

/// Headers for update responses, that are embedded as part of the payload of
/// an update response.
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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
