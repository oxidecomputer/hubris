// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]
#![deny(elided_lifetimes_in_paths)]
extern crate memoffset;

mod error;
pub use error::{SprocketsError, SprotError, SprotProtocolError};

use crc::{Crc, CRC_16_XMODEM};
pub use drv_update_api::{
    HandoffDataLoadError, RotBootState, RotSlot, SlotId, SwitchDuration,
    UpdateError, UpdateTarget,
};
use hubpack::SerializedSize;
use idol_runtime::{Leased, LenLimit, R};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
pub use sprockets_common::msgs::{
    RotRequestV1 as SprocketsReq, RotResponseV1 as SprocketsRsp,
};
use static_assertions::const_assert;
use userlib::sys_send;

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
pub const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
pub const ROT_FIFO_SIZE: usize = 8;
pub const MAX_BLOB_SIZE: usize = 512;
pub const MAX_REQUEST_SIZE: usize =
    Header::MAX_SIZE + ReqBody::MAX_SIZE + MAX_BLOB_SIZE;
pub const MAX_RESPONSE_SIZE: usize =
    Header::MAX_SIZE + RspBody::MAX_SIZE + MAX_BLOB_SIZE;

// For simplicity we want to be able to retrieve the header
// in a maximum of 1 FIFO size read.
const_assert!(Header::MAX_SIZE <= ROT_FIFO_SIZE);

pub type Request<'a> = Msg<'a, ReqBody, MAX_REQUEST_SIZE>;
pub type Response<'a> = Msg<'a, Result<RspBody, SprotError>, MAX_RESPONSE_SIZE>;

/// A message header for a request or response
///
/// It's important that this header be kept fixed size by limiting the use of
/// rust types in fields to those with fixed size serialization in Hubpack.
/// This essentially means only primitives, structs, or simple enums can be
/// used in fields. This allows us to assume that Header::MAX_SIZE is also the
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

/// A sprot Msg that flows between the SP and RoT
///
/// The message is parameterized by a `ReqBody` or `RspBody`.
///
/// Note that `MSG`s do not implement `Serialize`, `Deserialize`, or
/// `SerializedSize`, as they need to calculate and place a CRC in the buffer.
/// `Msg`s sometimes include a an offset into the buffer where a binary blob
/// resides.
pub struct Msg<'a, T, const N: usize> {
    pub header: Header,
    pub body: T,

    // The serialized buffer where an optional binary blob lives
    pub blob: &'a [u8],
}

impl<'a, T, const N: usize> Msg<'a, T, N>
where
    T: Serialize + DeserializeOwned + SerializedSize,
{
    /// Serialize a `Header` followed by a `ReqBody` or `RspBody`, compute a CRC, serialize
    /// the CRC, and return the total size of the serialized request.
    ///
    // Note that we unwrap instead of returning an error here because failure
    // to serialize is a programmer error rather than a runtime error.
    pub fn pack(body: &T, buf: &mut [u8; N]) -> usize {
        // Serialize `body`
        let mut size = hubpack::serialize(&mut buf[Header::MAX_SIZE..], body)
            .unwrap_lite();

        // Create a header, now that we know the size of the body
        let header = Header::new(size.try_into().unwrap_lite());

        // Serialize the header
        size += hubpack::serialize(buf, &header).unwrap_lite();

        // Compute and serialize the CRC
        let crc = CRC16.checksum(&buf[..size]);
        size += hubpack::serialize(&mut buf[size..], &crc).unwrap_lite();

        size
    }

    /// Serialize a `Header` followed by a `ReqBody` or `RspBody`, copy a blob
    /// into `buf` after the serialized body,  compute a CRC, serialize the
    /// CRC, and return the total size of the serialized request.
    pub fn pack_with_blob(
        body: &T,
        buf: &mut [u8; N],
        blob: LenLimit<Leased<R, [u8]>, MAX_BLOB_SIZE>,
    ) -> Result<usize, SprotProtocolError> {
        // Serialize `body`
        let mut size = hubpack::serialize(&mut buf[Header::MAX_SIZE..], body)
            .unwrap_lite();

        // Copy the blob into the buffer after the serialized body
        blob.read_range(0..blob.len(), &mut buf[Header::MAX_SIZE + size..])
            .map_err(|_| SprotProtocolError::ServerRestarted)?;

        size += blob.len();

        // Create a header, now that we know the size of the body
        let header = Header::new(size.try_into().unwrap_lite());

        // Serialize the header
        size += hubpack::serialize(buf, &header).unwrap_lite();

        // Compute and serialize the CRC
        let crc = CRC16.checksum(&buf[..size]);
        size += hubpack::serialize(&mut buf[size..], &crc).unwrap_lite();

        Ok(size)
    }

    // Deserialize and return a `Msg`
    pub fn unpack(buf: &'a [u8]) -> Result<Msg<'a, T, N>, SprotProtocolError> {
        let (header, rest) = hubpack::deserialize::<Header>(buf)?;
        if header.protocol != Protocol::V2 {
            return Err(SprotProtocolError::UnsupportedProtocol);
        }
        Self::unpack_body(header, buf, rest)
    }

    /// Deserialize just the body, given a header that was already deserialized.
    pub fn unpack_body(
        header: Header,
        // The buffer containing the entire serialized `Msg` including the `Header`
        buf: &[u8],
        // The part of the after the header buffer including the body and CRC
        rest: &'a [u8],
    ) -> Result<Msg<'a, T, N>, SprotProtocolError> {
        let (body, blob_buf) = hubpack::deserialize::<T>(rest)?;
        let end = Header::MAX_SIZE + header.body_size as usize;
        let (checksummed_part, tail) = buf.split_at(end);
        let computed_crc = CRC16.checksum(checksummed_part);

        // The CRC comes after the body, and is not included in header body_len
        let (crc, _) = hubpack::deserialize(tail)?;

        if computed_crc == crc {
            let blob_len =
                header.body_size as usize - (rest.len() - blob_buf.len());
            let blob = &blob_buf[..blob_len];
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
