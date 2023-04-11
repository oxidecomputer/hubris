// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]
extern crate memoffset;

use crc::{Crc, CRC_16_XMODEM};
use derive_more::From;
use drv_spi_api::SpiError;
pub use drv_update_api::{
    HandoffDataLoadError, RotBootState, RotSlot, SlotId, SwitchDuration,
    UpdateError, UpdateTarget,
};
use dumper_api::DumperError;
use hubpack::SerializedSize;
use idol_runtime::{Leased, LenLimit, RequestError, R};
use serde::{Deserialize, Serialize};
use sprockets_common::msgs::{
    RotError as SprocketsError, RotRequestV1 as SprocketsReq,
    RotResponseV1 as SprocketsRsp,
};
use static_assertions::const_assert;
use userlib::sys_send;

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
pub const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
// XXX ROT FIFO size should be discovered.
pub const ROT_FIFO_SIZE: usize = 8;
pub const MAX_BLOB_SIZE: usize = 512;
pub const MAX_REQUEST_SIZE: usize =
    Header::MAX_SIZE + ReqBody::MAX_SIZE + MAX_BLOB_SIZE;
pub const MAX_RESPONSE_SIZE: usize =
    Header::MAX_SIZE + RspBody::MAX_SIZE + MAX_BLOB_SIZE;

// For simplicity we want to be able to retrieve the header
// in a maximum of 1 FIFO size read.
const_assert!(Header::MAX_SIZE <= ROT_FIFO_SIZE);

pub type Request = Msg<ReqBody>;
pub type Response = Msg<Result<RspBody, SprotError>>;

/// A message header for a request or response
///
/// It's important that this header be kept fixed size by limiting the use
/// of rust types. This allows us to assume that Header::MAX_SIZE is also the
/// exact size of the header.
#[derive(Serialize, Deserialize, SerializedSize)]
pub struct Header {
    pub protocol: Protocol,
    pub body_size: u16,
}

impl Header {
    fn new(body_size: u16) -> Header {
        Header {
            protocol: Protocol::V2,
            body_size,
        }
    }
}

/// Information about optional blobs appended to the body of messages
/// before the CRC.
pub struct BlobInfo {
    pub offset: usize,
    pub size: usize,
}

/// A sprot Msg that flows between the SP and RoT
///
/// The message is parameterized by a `ReqBody` or `RspBody`.
///
/// Note that `MSG`s do not implement `Serialize`, `Deserialize`, or
/// `SerializedSize`, as they need to calculate and place a CRC in the buffer.
/// `Msg`s sometimes include a an offset into the buffer where a binary blob
/// resides.
pub struct Msg<T> {
    pub header: Header,
    pub body: T,

    // The index into the serialized buffer where an optional binary blob lives
    pub blob: Option<BlobInfo>,
}

impl<T> Msg<T>
where
    T: Serialize + for<'a> Deserialize<'a> + SerializedSize,
{
    /// Serialize a `Header` followed by a `ReqBody` or `RspBody`, compute a CRC, serialize
    /// the CRC, and return the total size of the serialized request.
    pub fn pack(body: &T, buf: &mut [u8]) -> Result<usize, SprotProtocolError> {
        // Serialize `body`
        let mut size = hubpack::serialize(&mut buf[Header::MAX_SIZE..], body)?;

        // Create a header, now that we know the size of the body
        let header = Header::new(size.try_into().unwrap_lite());

        // Serialize the header
        size += hubpack::serialize(buf, &header)?;

        // Compute and serialize the CRC
        let crc = CRC16.checksum(&buf[..size]);
        size += hubpack::serialize(&mut buf[size..], &crc)?;

        Ok(size)
    }

    /// Serialize a `Header` followed by a `ReqBody` or `RspBody, copy a blob
    /// into `buf` after the serialized body,  compute a CRC, serialize the
    /// CRC, and return the total size of the serialized request.
    pub fn pack_with_blob(
        body: &T,
        buf: &mut [u8],
        blob: LenLimit<Leased<R, [u8]>, MAX_BLOB_SIZE>,
    ) -> Result<usize, SprotProtocolError> {
        // Serialize `body`
        let mut size = hubpack::serialize(&mut buf[Header::MAX_SIZE..], body)?;

        // Copy the blob into the buffer after the serialized body
        blob.read_range(0..blob.len(), &mut buf[Header::MAX_SIZE + size..])
            .map_err(|_| SprotProtocolError::ServerRestarted)?;

        size += blob.len();

        // Create a header, now that we know the size of the body
        let header = Header::new(size.try_into().unwrap_lite());

        // Serialize the header
        size += hubpack::serialize(buf, &header)?;

        // Compute and serialize the CRC
        let crc = CRC16.checksum(&buf[..size]);
        size += hubpack::serialize(&mut buf[size..], &crc)?;

        Ok(size)
    }

    // Deserialize and return a `Msg`
    pub fn unpack(buf: &[u8]) -> Result<Msg<T>, SprotProtocolError> {
        let (header, body_buf) = hubpack::deserialize::<Header>(buf)?;
        if header.protocol != Protocol::V2 {
            return Err(SprotProtocolError::UnsupportedProtocol);
        }
        Self::unpack_body(header, buf, body_buf)
    }

    /// Deserialize just the body, given a header that was already deserialized.
    pub fn unpack_body(
        header: Header,
        // The buffer containing the entire serialized `Msg` including the `Header`
        buf: &[u8],
        // The body part of the buffer including the CRC at the end
        body_buf: &[u8],
    ) -> Result<Msg<T>, SprotProtocolError> {
        let (body, blob_buf) = hubpack::deserialize::<T>(body_buf)?;
        let end = Header::MAX_SIZE + header.body_size as usize;
        let blob_len =
            header.body_size as usize - (body_buf.len() - blob_buf.len());
        let computed_crc = CRC16.checksum(&buf[..end]);
        let blob = if blob_len != 0 {
            Some(BlobInfo {
                size: blob_len,
                offset: buf.len() - blob_buf.len(),
            })
        } else {
            None
        };

        // The CRC comes after the body, and is not included in header body_len
        let (crc, _) = hubpack::deserialize::<u16>(&buf[end..])?;
        if computed_crc == crc {
            Ok(Msg { header, body, blob })
        } else {
            Err(SprotProtocolError::InvalidCrc)
        }
    }
}

/// Protocol version
/// This is the first byte of any Sprot request or response
#[derive(
    Copy, Clone, Eq, PartialEq, Deserialize, Serialize, SerializedSize,
)]
#[repr(u8)]
pub enum Protocol {
    /// Indicates that no message is present.
    Ignore,
    /// The first sprot format with hand-rolled serialization.
    V1,
    /// The second format, using hubpack
    V2,
}

/// The body of a sprot request
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum ReqBody {
    Status,
    IoStats,
    Sprockets(SprocketsReq),
    Update(UpdateReq),
    Dump { addr: u32 },
}

/// A request used for RoT updates
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum UpdateReq {
    GetBlockSize,
    Prep(UpdateTarget),
    WriteBlock {
        block_num: u32,
    },
    SwitchDefaultImage {
        slot: SlotId,
        duration: SwitchDuration,
    },
    Finish,
    Abort,
    Reset,
}

/// A response used for RoT updates
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum UpdateRsp {
    BlockSize(u32),
}

/// The body of a sprot request
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum RspBody {
    // General Ok status shared among response variants
    Ok,
    Status(SprotStatus),
    IoStats(RotIoStats),
    Sprockets(SprocketsRsp),
    Update(UpdateRsp),
}

/// An error returned from a sprot request
#[derive(
    Copy, Clone, Serialize, Deserialize, SerializedSize, From, PartialEq,
)]
pub enum SprotError {
    Protocol(SprotProtocolError),
    Spi(SpiError),
    Update(UpdateError),
    Sprockets(SprocketsError),
    Dump(DumperError),
}

impl From<idol_runtime::ServerDeath> for SprotError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        SprotError::Protocol(SprotProtocolError::ServerRestarted)
    }
}

/// Sprot protocol specific errors
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
// Used by control-plane-agent
#[repr(u32)]
pub enum SprotProtocolError {
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
    // We cannot assert chip select
    CannotAssertCSn,
    // The request timed out
    Timeout,
    // Hubpack error: Used for both serialization and deserialization
    Serialization,
    // The RoT has not de-asserted ROT_IRQ
    RotIrqRemainsAsserted,
    // An unexpected response was received.
    // This should basically be impossible. We only include it so we can
    // return this error when unpacking a RspBody in idol calls.
    UnexpectedResponse,

    // Unexpected binary blob trailer received wtih message
    UnexpectedBlob,

    // Missing expected binary blob trailer received wtih message
    MissingBlob,

    // Failed to load update status
    BadUpdateStatus,

    // Used for mapping From<idol_runtime::ServerDeath>
    ServerRestarted,
}

impl From<SprotProtocolError> for RequestError<SprotError> {
    fn from(err: SprotProtocolError) -> Self {
        SprotError::from(err).into()
    }
}

impl From<hubpack::Error> for SprotError {
    fn from(_: hubpack::Error) -> Self {
        SprotProtocolError::Serialization.into()
    }
}

impl From<hubpack::Error> for SprotProtocolError {
    fn from(_: hubpack::Error) -> Self {
        SprotProtocolError::Serialization
    }
}

impl SprotError {
    pub fn is_recoverable(&self) -> bool {
        match *self {
            SprotError::Protocol(err) => {
                use SprotProtocolError::*;
                match err {
                    InvalidCrc | FlowError | Timeout | ServerRestarted
                    | Serialization => true,
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

/// The successful result of pulsing the active low chip-select line
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub struct PulseStatus {
    pub rot_irq_begin: u8,
    pub rot_irq_end: u8,
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
    /// TODO: This should live in the stage0 handoff info.
    pub bootrom_crc32: u32,

    /// Maxiumum request size that the RoT can handle.
    pub max_request_size: u32,

    /// Maximum response size returned from the RoT to the SP
    pub max_response_size: u32,

    pub rot_updates: RotBootState,
}

/// Stats from the RoT side of sprot
///
/// All of the counters will wrap around.
#[derive(
    Default, Clone, Copy, PartialEq, Serialize, Deserialize, SerializedSize,
)]
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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
