// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

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
use sprockets_common::msgs::{
    RotError as SprocketsError, RotRequestV1 as SprocketsReq,
    RotResponseV1 as SprocketsRsp,
};
use userlib::{sys_send, FromPrimitive};

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
// XXX ROT FIFO size should be discovered.
pub const ROT_FIFO_SIZE: usize = 8;

/// A sprot request from the SP to the RoT
pub struct Request {
    protocol: Protocol,
    body: ReqBody,
    crc: u16,
}

impl Request {
    /// Create a `Request` with `Protocol::V2` header, calculate a CRC16 over
    // the `protocol` and `body` fields, then serialize it into `buf` with
    // hubpack, returning the serialized size.
    pub fn pack(
        body: &ReqBody,
        buf: &mut [u8],
    ) -> Result<size, hubpack::Error> {
        buf[0] = Protocol::V2;
        let mut crc_start = buf.len() - CRC_SIZE;
        // Leave room for the Protocol byte  and CRC
        let size = hubpack::serialize(body, &mut buf[1..crc_start])?;
        crc_start = size + 1;
        let crc = CRC16.checksum(&buf[..crc_start]);
        let crc_buf = &mut buf[crc_start..][..2];
        let _ = hubpack::serialize(crc_buf, &crc).unwrap_lite();
        Ok(size + 1 + CRC_SIZE)
    }

    /// Deserialize a Request and validate its CRC
    pub fn unpack(buf: &[u8]) -> Result<Request, SprotProtocolError> {
        let protocol = Protocol::V2;
        if buf[0] != protocol {
            return Err(SprotProtocolError::UnsupportedProtocol);
        }
        let crc_start = buf.len() - 2;
        let (body, rest) = hubpack::deserialize(&buf[1..crc_start])?;
        let (crc, _) = hubpack::deserialize(rest)?;
        let computed = CRC16.checksum(&self.buf[..crc_start]);
        if computed == crc {
            Ok(Request {
                protocol,
                body,
                crc,
            })
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
pub struct Response {
    protocol: Protocol,
    // The SP needs to know how many bytes to clock in
    body_len: u16,
    body: Result<RspBody, SprotError>,
    crc: u16,
}

impl Response {
    /// Create a `Response` with `Protocol::V2` header, calculate a CRC16 over
    // the `length`, `protocol` and `body` fields, then serialize it into `buf` with
    // hubpack, returning the serialized size.
    pub fn pack(
        body: &RspBody,
        buf: &mut [u8],
    ) -> Result<size, hubpack::Error> {
        buf[0] = Protocol::V2;
        let mut crc_start = buf.len() - CRC_SIZE;
        // Protocol byte + u16 length
        let body_start = 3;

        // Serialize the body
        // Leave room for the Protocol byte, u16 length, and CRC
        let size = hubpack::serialize(body, &mut buf[body_start..crc_start])?;

        // Serialize the length of the body
        let _ = hubpack::serialize(
            &mut buf[1..body_start],
            &u16::try_from(size).unwrap_lite(),
        );
        crc_start = body_start + size;
        let crc = CRC16.checksum(&buf[..crc_start]);
        let crc_buf = &mut buf[crc_start..][..2];
        let _ = hubpack::serialize(crc_buf, &crc).unwrap_lite();
        Ok(body_start + size + CRC_SIZE)
    }

    /// Return the length of the entire serialized request, given a buffer of
    /// at least 3 bytes of the serialized request.
    pub fn parse_body_len(buf: &[u8]) -> Result<u16, SprotProtocolError> {
        assert!(buf.len() >= 3);
        if buf[0] != Protocol::V2 {
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
        assert!(buf.len() == body_len + 5);
        // Protocol byte + u16 length
        let body_start = 3;
        let crc_start = buf.len() - 2;
        let (body, rest) = hubpack::deserialize(&buf[body_start..])?;
        let (crc, _) = hubpack::deserialize(rest)?;
        let computed = CRC16.checksum(&self.buf[..crc_start]);
        if computed == crc {
            Ok(Response {
                protocol,
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
    Dump(DumpReq),
}

/// A request used for RoT updates
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum UpdateReq {
    GetBlockSize,
    Prep(SlotId),
    WriteBlock {
        block_num: u32,
        block: [u8; 512],
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
    BlockSize(usize),
}

/// The body of a sprot request
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum RspBody {
    // General Ok status shared among response variants
    Ok,
    Status(SprotStatus),
    IoStats(IoStats),
    Sprockets(SprocketsRsp),
    Update(UpdateRsp),
}

/// An error returned from a sprot request
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, SerializedSize)]
pub enum SprotError {
    Protocol(SprotProtocolError),
    Spi(SpiError),
    Update(UpdateError),
    Sprockets(SprocketsError),
}

/// Sprot protocol specific errors
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
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
    // Hubpack error
    Deserialization,
    // Hubpack error
    Serialization,
    // The RoT has not de-asserted ROT_IRQ
    RotIrqRemainsAsserted,
    // An explicit busy signal on the wire as the protocol byte
    RotBusy,
    // An unexpected response was received.
    // This should basically be impossible. We only include it so we can
    // return this error when unpacking a RspBody in idol calls.
    UnexpectedResponse,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<hubpack::Error> for SprotError {
    fn from(_: hubpack::Error) -> Self {
        SprotError::Deserialization
    }
}

impl SprotError {
    pub fn is_recoverable(&self) -> bool {
        use SprotError::*;
        match self {
            UnsupportedProtocol | CannotAssertCSn => false,
            _ => true,
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
