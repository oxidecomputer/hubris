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
use userlib::sys_send;

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
// XXX ROT FIFO size should be discovered.
pub const ROT_FIFO_SIZE: usize = 8;
pub const MAX_BLOB_SIZE: usize = 512;
pub const MAX_REQUEST_SIZE: usize =
    <Request as SerializedSize>::MAX_SIZE + MAX_BLOB_SIZE;
pub const MAX_RESPONSE_SIZE: usize = <Response as SerializedSize>::MAX_SIZE;

/// A sprot request from the SP to the RoT
#[derive(SerializedSize)]
pub struct Request {
    pub protocol: Protocol,
    pub body: ReqBody,
    // Optional binary data is stored after the CRC in the buffer. We can read
    // it directly out of the buffer without enlarging the size of ReqBody for
    // all variants and inducing an extra copy of the binary data.
    //
    // The blob is covered by the CRC if it exists
    pub blob_size: Option<u16>,
    pub crc: u16,
}

impl Request {
    /// Create a `Request` with `Protocol::V2` header, calculate a CRC16 over
    // the `protocol` and `body` fields, then serialize it into `buf` with
    // hubpack, returning the serialized size.
    pub fn pack(
        body: &ReqBody,
        buf: &mut [u8],
    ) -> Result<usize, SprotProtocolError> {
        buf[0] = Protocol::V2 as u8;

        // `protocol` byte
        let mut size = 1;

        // Serialize `body`
        size += hubpack::serialize(&mut buf[size..], body)?;

        // Serialize `blob_size`
        let blob_size: Option<u16> = None;
        size += hubpack::serialize(&mut buf[size..], &blob_size)?;

        // Calculate the CRC
        let crc = CRC16.checksum(&buf[..size]);

        // Serialize the CRC
        size += hubpack::serialize(&mut buf[size..], &crc).unwrap_lite();

        Ok(size)
    }

    pub fn pack_with_blob(
        body: &ReqBody,
        buf: &mut [u8],
        blob: LenLimit<Leased<R, [u8]>, MAX_BLOB_SIZE>,
    ) -> Result<usize, SprotProtocolError> {
        buf[0] = Protocol::V2 as u8;

        // `protocol` byte
        let mut size = 1;

        // Serialize `body`
        size += hubpack::serialize(&mut buf[size..], body)?;

        // Serialize `blob_size`
        size += hubpack::serialize(&mut buf[size..], &Some(blob.len()))?;

        // Leave room for the CRC
        let blob_start = size + 2;
        let blob_end = blob_start + blob.len();

        // Copy the blob into the buffer
        blob.read_range(0..blob.len(), &mut buf[blob_start..])
            .map_err(|_| SprotProtocolError::ServerRestarted)?;

        // Calculate the CRC
        let mut digest = CRC16.digest();
        digest.update(&buf[..size]);
        digest.update(&buf[blob_start..blob_end]);
        let crc = digest.finalize();

        // Serialize the CRC
        size += hubpack::serialize(&mut buf[size..], &crc).unwrap_lite();

        Ok(size + blob.len())
    }

    /// Deserialize a Request and validate its CRC
    /// Return an offset to the start of the blob from `buf`, if a blob exists.
    pub fn unpack(
        buf: &[u8],
    ) -> Result<(Request, Option<usize>), SprotProtocolError> {
        let protocol = Protocol::V2;
        if buf[0] != protocol as u8 {
            return Err(SprotProtocolError::UnsupportedProtocol);
        }
        let (body, rest) = hubpack::deserialize(&buf[1..])?;
        let (blob_size, crc_start) = hubpack::deserialize(rest)?;
        let (crc, blob_start) = hubpack::deserialize(crc_start)?;

        let crc_offset = buf.len() - crc_start.len();

        let (computed, blob_offset) = if let Some(size) = blob_size {
            let mut digest = CRC16.digest();
            digest.update(&buf[..crc_offset]);
            digest.update(&blob_start[..size as usize]);
            let crc = digest.finalize();
            let blob_offset = buf.len() - blob_start.len();
            (crc, Some(blob_offset))
        } else {
            (CRC16.checksum(&buf[..crc_offset]), None)
        };

        if computed == crc {
            let request = Request {
                protocol,
                body,
                blob_size,
                crc,
            };
            Ok((request, blob_offset))
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

/// A sprot response from the RoT to the SP
#[derive(SerializedSize)]
pub struct Response {
    pub protocol: Protocol,
    // The SP needs to know how many bytes to clock in
    pub body_len: u16,
    pub body: Result<RspBody, SprotError>,
    pub crc: u16,
}

impl Response {
    /// Create a `Response` with `Protocol::V2` header, calculate a CRC16 over
    // the `length`, `protocol` and `body` fields, then serialize it into `buf` with
    // hubpack, returning the serialized size.
    pub fn pack(body: Result<RspBody, SprotError>, buf: &mut [u8]) -> usize {
        buf[0] = Protocol::V2 as u8;
        let mut crc_start = buf.len() - CRC_SIZE;
        // Protocol byte + u16 length
        let body_start = 3;

        // Serialize the body
        // Leave room for the Protocol byte, u16 length, and CRC
        // We treat failure as a programmer error, as the buffer should
        // always be sized large enough.
        let size = hubpack::serialize(&mut buf[body_start..crc_start], &body)
            .unwrap_lite();

        // Serialize the length of the body
        let _ = hubpack::serialize(
            &mut buf[1..body_start],
            &u16::try_from(size).unwrap_lite(),
        );
        crc_start = body_start + size;
        let crc = CRC16.checksum(&buf[..crc_start]);
        let crc_buf = &mut buf[crc_start..][..2];
        let _ = hubpack::serialize(crc_buf, &crc).unwrap_lite();
        body_start + size + CRC_SIZE
    }

    /// Return the length of the entire serialized request, given a buffer of
    /// at least 3 bytes of the serialized request.
    pub fn parse_body_len(buf: &[u8]) -> Result<u16, SprotProtocolError> {
        assert!(buf.len() >= 3);
        if buf[0] != Protocol::V2 as u8 {
            return Err(SprotProtocolError::UnsupportedProtocol);
        }
        let (body_len, _) = hubpack::deserialize(&buf[1..])?;

        Ok(body_len)
    }

    /// Return the total size of a serialized response given its body length
    pub fn total_len(body_len: u16) -> usize {
        // 5 = protocol byte + u16 length + u16 crc
        body_len as usize + 5
    }

    /// Deserialize a Response and validate its CRC.
    ///
    /// The buffer passed in must be the exact buffer size of the received message.
    ///
    /// This operates on the whole buffer in order to validate the CRC, but
    /// does not reparse the protocol byte or body length as those were already
    /// parsed.
    pub fn unpack_remaining(
        buf: &[u8],
        body_len: u16,
    ) -> Result<Response, SprotProtocolError> {
        // Protocol byte + u16 length + u16 CRC
        assert!(buf.len() == body_len as usize + 5);
        // Protocol byte + u16 length
        let body_start = 3;
        let crc_start = buf.len() - 2;
        let (body, rest) = hubpack::deserialize(&buf[body_start..])?;
        let (crc, _) = hubpack::deserialize(rest)?;
        let computed = CRC16.checksum(&buf[..crc_start]);
        if computed == crc {
            Ok(Response {
                protocol: Protocol::V2,
                body_len,
                body,
                crc,
            })
        } else {
            Err(SprotProtocolError::InvalidCrc)
        }
    }
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
                    InvalidCrc | FlowError | Timeout | ServerRestarted => true,
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

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
