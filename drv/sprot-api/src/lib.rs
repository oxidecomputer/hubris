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
use drv_spi_api::SpiError;
pub use drv_update_api::{
    HandoffDataLoadError, RotBootState, RotSlot, SlotId, SwitchDuration,
    UpdateError, UpdateTarget,
};
use hubpack::SerializedSize;
use idol_runtime::{Leased, R};
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};
use zerocopy::{AsBytes, FromBytes};
#[cfg(feature = "sink_test")]
use zerocopy::{ByteOrder, LittleEndian};

/// The canonical SpRot protocol error returned by this API
//
// Audit that each MsgError is used and has some reasonable action.
// While a diverse set of error codes may be useful for debugging it
// clutters code that just has to deal with the error.
// then consider adding a function that translates an error code
// into the desired action, e.g. InvalidCrc and FlowError should both
// result in a retry on the SP side and an ErrorRsp on the RoT side.
// Note: Variant order must be maintained to stay compatible due to SP/RoT
// version skew during update.
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
    BadTransferSize,
    /// A task crashed during an operation, commonly a lease read or write
    TaskRestart,
    /// CRC check failed.
    InvalidCrc,
    /// FIFO overflow/underflow
    FlowError,
    /// Unsupported protocol version
    UnsupportedProtocol,
    /// Unknown message
    BadMessageType,
    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadMessageLength,
    /// Error from Spi
    SpiServerError,
    /// Message is too large
    Oversize,
    /// Tx buffer is unexpectedly not Idle.
    TxNotIdle,

    // SPI-related errors
    SpiServerLockError,
    SpiServerWritePart1Error,
    SpiServerWritePart2Error,
    SpiServerReleaseError,
    SpiServerReadPart1Error,
    SpiServerReadPart2Error,
    CannotAssertCSn,
    RotNotReady,
    // SP initiated RoT reset request should timeout.
    RspTimeout,
    BadResponse,
    RotBusy,

    /// Feature is not implemented
    NotImplemented,
    /// An error code reserved for the SP was used by the Rot
    NonRotError,
    /// A message with version = 0 was received unexpectedly.
    EmptyMessage,

    /// Insufficient bytes received
    Incomplete,
    /// Hubpack error
    Serialization,
    /// Sequence number mismatch in Sink test
    Sequence,

    //
    // Update Related Errors
    //
    UpdateBadLength,
    UpdateInProgress,
    UpdateOutOfBounds,
    UpdateTimeout,
    UpdateEccDoubleErr,
    UpdateEccSingleErr,
    UpdateSecureErr,
    UpdateReadProtErr,
    UpdateWriteEraseErr,
    UpdateInconsistencyErr,
    UpdateStrobeErr,
    UpdateProgSeqErr,
    UpdateWriteProtErr,
    UpdateBadImageType,
    UpdateAlreadyFinished,
    UpdateNotStarted,
    UpdateRunningImage,
    UpdateFlashError,
    UpdateSpRotError,
    UpdateMissingHeaderBlock,
    UpdateInvalidHeaderBlock,
    UpdateServerRestarted,
    UpdateUnknown,
    // The status was returned for the SP, which is not what we asked about
    UpdateBadStatus,

    // An error relating to Stage0 handoff of image data
    Stage0HandoffError,

    #[idol(server_death)]
    ServerRestarted,

    // Used if no explicit error code is available.
    Unknown,

    // Rot sink test not configured
    RotSinkTestNotConfigured,
    UpdateNotImplemented,
}

impl From<SpiError> for SprotError {
    fn from(_value: SpiError) -> Self {
        SprotError::SpiServerError
    }
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
            UpdateError::MissingHeaderBlock => {
                SprotError::UpdateMissingHeaderBlock
            }
            UpdateError::InvalidHeaderBlock => {
                SprotError::UpdateInvalidHeaderBlock
            }
            UpdateError::ServerRestarted => SprotError::UpdateServerRestarted,
            UpdateError::Unknown
            | UpdateError::ImageBoardMismatch // TODO add new error codes here
            | UpdateError::ImageBoardUnknown => SprotError::UpdateUnknown,
            UpdateError::NotImplemented => SprotError::UpdateNotImplemented,
        }
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
            | SprotError::RspTimeout
            | SprotError::FlowError
            | SprotError::UnsupportedProtocol
            | SprotError::BadMessageLength
            | SprotError::Serialization
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
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct SprotStatus {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,

    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    /// This should live in the stage0 handoff info.
    pub bootrom_crc32: u32,

    /// Maxiumum message size that the RoT can handle.
    pub buffer_size: u32,

    pub rot_updates: RotBootState,
}

/// Stats from the RoT side of sprot
///
/// All of the counters will wrap around.
#[derive(Default, Clone, Serialize, Deserialize, SerializedSize)]
pub struct RotIoStats {
    /// Number of messages received
    pub rx_received: u32,

    /// Number of messages where the RoT failed to service the Rx FIFO in time.
    pub rx_overrun: u32,

    /// The number of times an SP sent more bytes than expected for one
    /// message. In otherwords, the number of bytes sent by the SP to the RoT
    /// between CSn assert and CSn de-assert exceeds `BUF_SIZE`.
    pub rx_protocol_error_too_many_bytes: u32,

    /// The number of CSn pulses seen by the RoT
    pub csn_pulses: u32,

    /// Number of messages where the RoT failed to service the Tx FIFO in time.
    pub tx_underrun: u32,

    /// Number of invalid messages received
    pub rx_invalid: u32,

    /// Number of incomplete transmissions (valid data not fetched by SP).
    pub tx_incomplete: u32,
}

/// Stats from the SP side of sprot
///
/// All of the counters will wrap around.
#[derive(
    Default, Copy, Clone, PartialEq, Serialize, Deserialize, SerializedSize,
)]
pub struct SpIoStats {
    // Number of messages sent successfully
    pub tx_sent: u32,

    // Number of messages that failed to be sent
    pub tx_errors: u32,

    // Number of messages received successfully
    pub rx_received: u32,

    // Number of error replies received
    pub rx_errors: u32,

    // Number of invalid messages received. They don't parse properly.
    pub rx_invalid: u32,

    // Total Number of retries issued
    pub retries: u32,

    // Number of times the SP pulsed CSn
    pub csn_pulses: u32,

    // Number of times pulsing CSn failed.
    pub csn_pulse_failures: u32,

    // Number of timeouts, while waiting for a reply
    pub timeouts: u32,
}

/// Sprot related stats
#[derive(Default, Clone, Serialize, Deserialize, SerializedSize)]
pub struct IoStats {
    pub rot: RotIoStats,
    pub sp: SpIoStats,
}

/// Switch Default Image payload
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub struct SwitchDefaultImageHeader {
    pub slot: SlotId,
    pub duration: SwitchDuration,
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
// The value of MsgType is seen on the wire.
// The explicit value assignments are to aid lookup when debugging.
//
// The current on-the-wire format is:
//
//   {Protocol::V1, MsgType, Length, Payload, CRC}
//
// It has been suggested that the MsgType could be folded into the
// hubpack encoded payload so the message would then be:
//
//   {Protocol::V2, Length, Payload{MsgType, msg-specific-payload}, CRC}
//
// In that case, hubpack would control the on-the-wire encoding of MsgType
// and the explicit assignments below could be misleading.
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
    StatusRsp = 5,
    /// Payload contains a sprockets request.
    SprocketsReq = 6,
    /// Payload contains a sprockets response.
    SprocketsRsp = 7,
    /// RoT sinks this message sending back a SinkRsp with no payload.
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

    // Dump
    DumpReq = 22,
    DumpRsp = 23,

    // Switch Default Image
    UpdSwitchDefaultImageReq = 24,
    // Reset
    UpdSwitchDefaultImageRsp = 25,
    UpdResetReq = 26,
    UpdResetRsp = 27,
}

/// A builder/serializer for messages that wraps the transmit buffer
///
pub struct TxMsg<'a> {
    buf: &'a mut [u8],
}

impl<'a> TxMsg<'a> {
    /// Wrap `buf`, and zero it.
    pub fn new(buf: &'a mut [u8]) -> TxMsg<'a> {
        assert_eq!(buf.len(), BUF_SIZE);
        // Ensure we start with a zero filled buffer
        buf.fill(0);
        TxMsg { buf }
    }

    /// Serialize an ErrorRsp with a one byte payload, which is a serialized
    /// `SprotError`
    pub fn error_rsp(self, err: SprotError) -> VerifiedTxMsg<'a> {
        let payload_size = 1;
        self.buf[HEADER_SIZE] = err as u8;
        self.from_existing(MsgType::ErrorRsp, payload_size)
            .unwrap_lite()
    }

    /// Serialize a request with no payload
    pub fn no_payload(mut self, msgtype: MsgType) -> VerifiedTxMsg<'a> {
        let payload_size = 0;
        self.write_header(msgtype, payload_size);
        self.write_crc(msgtype, payload_size)
    }

    /// Fill the payload with a known pattern of `size` bytes of data generated
    /// and include a sequence number as the first 2 bytes of the payload. The
    /// sequence number requirement necessitates that `size > 2`.
    ///
    /// Then serialize the request via `from_existing`, which writes the header
    /// and trailing CRC.
    ///
    /// For the sake of working with a logic analyzer, a known pattern is put
    /// into the SinkReq messages so that most of the received bytes match
    /// their buffer index modulo 0x100.
    #[cfg(feature = "sink_test")]
    pub fn sink_req(
        mut self,
        size: usize,
        seq_num: u16,
    ) -> Result<VerifiedTxMsg<'a>, SprotError> {
        let seq_num_size = core::mem::size_of::<u16>();
        if size > PAYLOAD_SIZE_MAX || size < seq_num_size {
            return Err(SprotError::BadMessageLength);
        }
        let buf = &mut self.payload_mut()[..size];

        // Fill the payload with a known pattern
        let mut n: u8 = HEADER_SIZE as u8;
        buf.fill_with(|| {
            let seq = n;
            n = n.wrapping_add(1);
            seq
        });

        // Overwrite the first two bytes with a sequence number.
        let seq_buf = &mut buf[..seq_num_size];
        LittleEndian::write_u16(seq_buf, seq_num);

        self.from_existing(MsgType::SinkReq, size)
            .map_err(|(_, e)| e)
    }

    /// Serialize an arbitrary message, consuming self
    ///
    /// If there is an error, return it along with self
    pub fn serialize<T: Serialize>(
        mut self,
        msgtype: MsgType,
        msg: T,
    ) -> Result<VerifiedTxMsg<'a>, (Self, SprotError)> {
        match hubpack::serialize(self.payload_mut(), &msg) {
            Ok(n) => self.from_existing(msgtype, n),
            Err(e) => Err((self, e.into())),
        }
    }

    /// Return the mutable payload buffer
    //
    // TODO(AJS): Make this private if possible
    // It's currently used publicly on the RoT side for Echo and Sprockets
    // messages along with `from_existing`, and for update messages on the
    // SP side.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.buf[HEADER_SIZE..BUF_SIZE - CRC_SIZE]
    }

    /// Serialize a block
    ///
    /// Each block is prefixed by its block num serialized as a u32
    pub fn block(
        mut self,
        block_num: u32,
        block: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            1024,
        >,
    ) -> Result<VerifiedTxMsg<'a>, SprotError> {
        let n = hubpack::serialize(self.payload_mut(), &block_num)?;
        block
            .read_range(
                0..block.len(),
                &mut self.payload_mut()[n..n + block.len()],
            )
            .map_err(|_| SprotError::TaskRestart)?;
        let payload_len = n + block.len();
        self.from_existing(MsgType::UpdWriteOneBlockReq, payload_len)
            .map_err(|(_, e)| e)
    }

    /// Serialize a SwitchDefaultImage request
    pub fn switch_default_image(
        mut self,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<VerifiedTxMsg<'a>, SprotError> {
        let payload_len = hubpack::serialize(
            self.payload_mut(),
            &SwitchDefaultImageHeader { slot, duration },
        )?;
        self.from_existing(MsgType::UpdSwitchDefaultImageReq, payload_len)
            .map_err(|(_, e)| e)
    }

    /// Serialize a request from a MsgType and Lease
    pub fn from_lease(
        mut self,
        msgtype: MsgType,
        source: Leased<R, [u8]>,
    ) -> Result<VerifiedTxMsg<'a>, SprotError> {
        if source.len() > PAYLOAD_SIZE_MAX {
            return Err(SprotError::Oversize);
        }

        let dest = &mut self.buf[HEADER_SIZE..][..source.len()];
        source
            .read_range(0..source.len(), dest)
            .map_err(|_| SprotError::TaskRestart)?;

        self.write_header(msgtype, source.len());
        Ok(self.write_crc(msgtype, source.len()))
    }

    /// Serialize a request into `self.buf` with an already written payload
    /// inside `self.buf`.
    // TODO(AJS): Make this private if possible
    // It's currently used publicly for Echo and Sprockets messages along with
    // `payload_mut`.
    // Sprockets in particular just wants a buffer to write into.
    pub fn from_existing(
        mut self,
        msgtype: MsgType,
        payload_size: usize,
    ) -> Result<VerifiedTxMsg<'a>, (Self, SprotError)> {
        if payload_size > PAYLOAD_SIZE_MAX {
            return Err((self, SprotError::Oversize));
        }
        self.write_header(msgtype, payload_size);
        Ok(self.write_crc(msgtype, payload_size))
    }

    fn write_header(&mut self, msgtype: MsgType, payload_size: usize) {
        let _ = MsgHeader::new_v1(msgtype, payload_size)
            .unwrap_lite()
            .serialize(&mut self.buf[..])
            .unwrap_lite();
    }

    fn write_crc(
        self,
        msgtype: MsgType,
        payload_size: usize,
    ) -> VerifiedTxMsg<'a> {
        let crc_begin = HEADER_SIZE + payload_size;
        let msg_bytes = &self.buf[0..crc_begin];
        let crc = CRC16.checksum(msg_bytes);
        let end = crc_begin + CRC_SIZE;
        let crc_buf = &mut self.buf[crc_begin..end];
        let _ = hubpack::serialize(crc_buf, &crc).unwrap_lite();

        // Include the whole buffer, including trailing zeroes, so we can
        // convert back into a `TxMsg` backing the full buffer size.
        VerifiedTxMsg::new(msgtype, self.buf, end)
    }
}

pub struct VerifiedTxMsg<'a> {
    msgtype: Option<MsgType>,
    data: &'a mut [u8],

    // The amount of data written into the buffer,
    // Followed by zeroes if `len < data.len()`.
    len: usize,
}

// A fully serialized message
impl<'a> VerifiedTxMsg<'a> {
    // A data containing buffer can only be created by a TxMsg.
    fn new(
        msgtype: MsgType,
        data: &'a mut [u8],
        len: usize,
    ) -> VerifiedTxMsg<'a> {
        VerifiedTxMsg {
            msgtype: Some(msgtype),
            data,
            len,
        }
    }

    pub fn msgtype(&self) -> Option<MsgType> {
        self.msgtype
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        self.as_slice().iter().cloned()
    }

    pub fn into_txmsg(self) -> TxMsg<'a> {
        TxMsg::new(self.data)
    }
}

pub struct BufFull;

/// A parser/deserializer for messages received over SPI
pub struct RxMsg<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> RxMsg<'a> {
    pub fn new(buf: &'a mut [u8]) -> RxMsg<'a> {
        assert_eq!(buf.len(), BUF_SIZE);
        buf.fill(0);
        RxMsg { buf, len: 0 }
    }

    // Necessary for retries on the SP, since `do_send_recv_retries`
    // does not take ownership of the `RxMsg`.
    pub fn clear(&mut self) {
        self.buf.fill(0);
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, b: u8) -> Result<(), BufFull> {
        match self.buf.get_mut(self.len) {
            Some(x) => *x = b,
            None => return Err(BufFull),
        }
        self.len += 1;
        Ok(())
    }

    pub fn protocol(&self) -> Option<Protocol> {
        if self.len >= 1 {
            Some(Protocol::from(self.buf[0]))
        } else {
            None
        }
    }

    /// Return the first byte of the payload that indicates an error
    /// This should only be used after a successful non-consuming parse, or
    /// else the data will be meaningless.
    pub fn payload_error_byte(&self) -> u8 {
        assert!(self.len > MIN_MSG_SIZE);
        self.buf[HEADER_SIZE]
    }

    /// Read `len` data into the underlying buffer at the current offset
    /// given a closure that takes the buffer. This method assumes
    /// that `len` bytes are  written after the closure returns. If `len` bytes
    /// are not available in `self.buf`, or `f` fails then an error is returned.
    pub fn read<F: FnMut(&mut [u8]) -> Result<(), SprotError>>(
        &mut self,
        len: usize,
        mut f: F,
    ) -> Result<(), SprotError> {
        if self.buf.len() - self.len < len {
            return Err(SprotError::BadMessageLength);
        }
        let buf = &mut self.buf[self.len..][..len];
        f(buf)?;
        self.len += len;
        Ok(())
    }

    /// Return an array containing the actual header bytes received so far
    /// and 0 bytes for any not filled.
    pub fn header_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        let end = core::cmp::min(self.len, HEADER_SIZE);
        buf[..end].copy_from_slice(&self.buf[..end]);
        buf
    }

    // Parse just the header and return it, or an error.
    pub fn parse_header(&self) -> Result<MsgHeader, SprotError> {
        // We want to be able to return `RotBusy` before `Incomplete`.
        self.parse_protocol()?;
        if self.len < HEADER_SIZE {
            return Err(SprotError::Incomplete);
        }
        let (header, _) = hubpack::deserialize::<MsgHeader>(self.buf)?;
        if header.payload_len as usize > PAYLOAD_SIZE_MAX {
            return Err(SprotError::BadMessageLength);
        }
        Ok(header)
    }

    /// Parse the header, validate the CRC, and returned a VerifiedRxMsg.
    /// Return the header_bytes along with a SprotError on error.
    pub fn parse(
        self,
    ) -> Result<VerifiedRxMsg<'a>, ([u8; HEADER_SIZE], SprotError)> {
        // We want to be able to return `RotBusy` before `Incomplete`.
        self.parse_protocol()
            .map_err(|e| (self.header_bytes(), e))?;
        if self.len < HEADER_SIZE {
            return Err((self.header_bytes(), SprotError::Incomplete));
        }

        let (header, _) = hubpack::deserialize::<MsgHeader>(self.buf)
            .map_err(|e| (self.header_bytes(), e.into()))?;
        if header.payload_len as usize > PAYLOAD_SIZE_MAX {
            return Err((self.header_bytes(), SprotError::BadMessageLength));
        }

        self.validate_crc(&header)
            .map_err(|e| (self.header_bytes(), e))?;

        Ok(VerifiedRxMsg {
            header,
            buf: self.buf,
        })
    }

    /// On the SP errors can elicit retries, which must reuse the receive
    /// buffer. Because of this, we make the `validate_crc` method public.
    /// On the RoT, the `parse` method should be used directly, as it is more
    /// robust and the conditions that necessitate this method on the SP are
    /// unnecessary.
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

// A fully parsed message with a validated crc
pub struct VerifiedRxMsg<'a> {
    header: MsgHeader,

    // The full BUF_SIZE, so that this can be converted back into an RxMsg
    buf: &'a mut [u8],
}

impl<'a> VerifiedRxMsg<'a> {
    pub fn header(&self) -> MsgHeader {
        self.header
    }

    pub fn payload(&self) -> &[u8] {
        let payload_end = HEADER_SIZE + self.header.payload_len as usize;
        &self.buf[HEADER_SIZE..payload_end]
    }

    /// Deserialize a hubpack encoded message, `M`, from the wrapped buffer
    pub fn deserialize_hubpack_payload<M>(&self) -> Result<M, SprotError>
    where
        M: for<'de> Deserialize<'de>,
    {
        let (msg, _) = hubpack::deserialize::<M>(self.payload())?;
        Ok(msg)
    }

    pub fn into_rxmsg(self) -> RxMsg<'a> {
        RxMsg::new(self.buf)
    }
}

// The SpRot Header prepended to each message traversing the SPI bus
// between the RoT and SP.
#[derive(
    Copy, Clone, Serialize, Deserialize, SerializedSize, PartialEq, Eq,
)]
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
pub const ROT_FIFO_SIZE: usize = 16;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
