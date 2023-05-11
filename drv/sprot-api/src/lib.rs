// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]
#![deny(elided_lifetimes_in_paths)]
extern crate memoffset;

mod error;
use dumper_api::DumperError;
pub use error::{
    DumpOrSprotError, SprocketsError, SprotError, SprotProtocolError,
};

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
pub const ROT_FIFO_SIZE: usize = 16; // bytes
pub const MAX_BLOB_SIZE: usize = 512;

/// The minimum version supported by this code. Messages older than this
/// minimum version will be served an error.
pub const MIN_VERSION: Version = Version(2);

/// The current version of this code
///
/// Code between the `CURRENT_VERSION` and `MIN_VERSION` must remain
/// compatible. Use the rules described in the comments for [`Msg`] to evolve
/// the protocol such that this remains true.
pub const CURRENT_VERSION: Version = Version(3);

/// We allow room in the buffer for message evolution
pub const REQUEST_BUF_SIZE: usize = 1024;
// We add 1 byte for padding a maximum sized message to an even number of bytes
// if necessary.
const_assert!(
    REQUEST_BUF_SIZE
        >= Header::MAX_SIZE + ReqBody::MAX_SIZE + MAX_BLOB_SIZE + CRC_SIZE + 1
);

/// We allow room in the buffer for message evolution
pub const RESPONSE_BUF_SIZE: usize = 1024;
// We add 1 byte for padding a maximum sized message to an even number of bytes
// if necessary.
const_assert!(
    RESPONSE_BUF_SIZE
        >= Header::MAX_SIZE + RspBody::MAX_SIZE + MAX_BLOB_SIZE + CRC_SIZE + 1
);

// For simplicity we want to be able to retrieve the header
// in a maximum of 1 FIFO size read.
const_assert!(Header::MAX_SIZE <= ROT_FIFO_SIZE);

/// A request from the SP to the RoT
pub type Request<'a> = Msg<'a, ReqBody, REQUEST_BUF_SIZE>;

/// A resposne from the RoT to the SP
pub type Response<'a> = Msg<'a, Result<RspBody, SprotError>, RESPONSE_BUF_SIZE>;

/// A message header for a request or response
#[derive(Serialize, Deserialize, SerializedSize)]
pub struct Header {
    pub version: Version,
    pub body_size: u16,
}

impl Header {
    fn new(body_size: u16) -> Header {
        Header {
            version: CURRENT_VERSION,
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
/// `Msg`s sometimes include an offset into the buffer where a binary blob
/// resides.
///
/// Messages are hubpack encoded and therefore the fields and enum
/// variants must *not* be reordered, or removed. In general, this allows
/// extensibility by adding fields or variants to the end of existing message
/// components. Details are provided below.
///
/// Backwards and Forwards Compatibility
/// ===========================================================================
///
/// Enum variant additions
/// ---------------------------------------------------------------------------
/// We can always add a new enum variant and be sure that old code will return
/// an error if sent that variant. There is no harm in this. New code will
/// always understand old messages since the encoding for older variants
/// doesn't change. Enum variant additions, when added to the bottom of an
/// existing enum, are therefore always forwards and backwards compatible.
///
///
/// Struct field additions
/// ---------------------------------------------------------------------------
/// Unfortunately the same is not true of adding new fields to structs. Adding
/// a new field to an existing struct increases the size of the struct on the
/// wire. A struct with a new field sent to an old version of the code will
/// still deserialize properly, as long as it is the *last* top-level struct
/// in the message. This is because the extra fields will be ignored.
///
/// If the struct is not at the top level but embedded in another struct, and a
/// field is added to the internal struct, old code will improperly deserialize
/// the data. This is because the old code will treat the extra field in the
/// internal struct as a later field in the outer struct not realizing that
/// the size of the inner struct has changed. This will not be a problem if the
/// inner struct is always the last field of the outer struct. In this scenario
/// the deserialization behavior will be the same as appending a field to the
/// outer struct.
///
/// Forwards compatibility is always a problem when adding a field to an
/// existing struct. Newer code expecting the extra field will fail to
/// deserialize a message from older code lacking that field without special
/// consideration.
///
/// Lastly, adding a field to a struct for any message that is expected to
/// have a blob appended to it will not work at all. Old code will think that
/// the blob starts at the new field offset, while new code will see a message
/// without an expected field and think the blob starts at a later offset,
/// interpreting part of the blob as the expected field.
///
///
/// Rules for message evolution
/// ---------------------------------------------------------------------------
/// Our primary goal when updating the sprot protocol messages is to minimize
/// version specific code to deal with compatibility issues. To keep the total
/// number of messages down we don't want to strictly prohibit version specific
/// message conversion code. However, we want to be judicious about when we use
/// this escape hatch. To make things concrete for the people working on  the
/// sprot protocol, we can devise a few rules that will allow us to meet our
/// goals and not end up in a mess of confusion.
///
/// 1. Any time `ReqBody` or `RspBody` changes increment the current version.
/// 2. Never remove, or re-order enum variants or fields in messages. Instead
/// mark them as deprecated in a comment (for now).
/// 2. If there is a semantic change to a message required, add it as a new
/// enum variant.
/// 3. If a message inside an enum variant is a 3rd party type not defined in
/// this file and it changes, add it as a new enum variant.
/// 4. Only flat structs defined in this file and at the bottom (leaf) of a
/// message enum hierarchy can have field additions. The field additions must
/// not change the semantic meaning of the message. This is really only useful
/// for things like `IoStats` where new information can be made available over
/// time.
/// 5. If a struct field addition is made, then custom code must be written in
/// the newer code to handle messages that do not contain this field. In almost
/// all cases, the new field should be wrapped in an `Option` and set to `None`
/// on receipt of an older version of the message. No special care must be
/// taken on the old code side since new fields will be ignored.
/// 6. Messages that carry a blob may not ever have struct field additions. A
/// new enum variant must be added in this case.
/// 7. If the blob of a message changes in type, size, semantics, etc.. a new
/// message should be added. Use your judgement here.
/// 8. Any time a new field, enum variant, or struct enclosed in a variant
/// is added, a comment should be added documenting what version it was added
/// in. Ideally there would be a procedural macro for this that would let
/// us generate some validation code and tests for compatibility. For now a
/// comment will suffice.
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
            .map_err(|_| SprotProtocolError::TaskRestarted)?;

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
        if header.version < MIN_VERSION {
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
    Debug,
    Copy,
    Clone,
    Eq,
    PartialEq,
    PartialOrd,
    Ord,
    Deserialize,
    Serialize,
    SerializedSize,
)]
pub struct Version(pub u32);

/// The body of a sprot request.
///
/// See [`Msg`] for details about versioning and message evolution.
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum ReqBody {
    Status,
    IoStats,
    RotState,
    Update(UpdateReq),
    Sprockets(SprocketsReq),
    Dump(DumpReq),
    Caboose(CabooseReq),
}

/// Instruct the RoT to take a dump of the SP via SWD
//
// Separate this into its own enum to allow better extensibility
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum DumpReq {
    V1 { addr: u32 },
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

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CabooseReq {
    Size { slot: u16 },
    Read { slot: u16, start: u32, size: u32 },
}

/// A response used for RoT updates
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum UpdateRsp {
    BlockSize(u32),
}

/// A response used for caboose requests
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CabooseRsp {
    Size(u32),
    Read,
}

/// The body of a sprot response.
///
/// See [`Msg`] for details about versioning and message evolution.
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum RspBody {
    // General Ok status shared among response variants
    Ok,
    // The RoT can only return `RotStatus`
    //
    // We fill in the `SpStatus` and return `SprotStatus` for the Idol
    // interface in the stm32h7-sprot-server
    Status(RotStatus),
    // The RoT can only return `RotIoStats`
    //
    // We fill in the `SpIoStats` and return `IoStats` for the Idol
    // interface in the stm32h7-sprot-server
    IoStats(RotIoStats),
    RotState(RotState),
    Update(UpdateRsp),
    Sprockets(SprocketsRsp),
    Dump(DumpRsp),
    Caboose(Result<CabooseRsp, CabooseErr>),
}

/// Minimal error type for caboose actions
///
/// This has some overlap with `drv_caboose::CabooseError`, but is versioned
/// according to the rules described in [`Msg`] and doesn't expose fine-grained
/// read errors.
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CabooseErr {
    MissingCaboose,
    NoSuchTag,
    ReadFailed,
}

impl From<CabooseErr> for drv_caboose::CabooseError {
    fn from(s: CabooseErr) -> Self {
        match s {
            CabooseErr::MissingCaboose => Self::MissingCaboose,
            CabooseErr::NoSuchTag => Self::NoSuchTag,
            CabooseErr::ReadFailed => Self::ReadFailed,
        }
    }
}

/// A response from the Dumper
//
// Separate this into its own enum to allow better extensibility
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum DumpRsp {
    V1 { err: Option<DumperError> },
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
    pub rot: RotStatus,
    pub sp: SpStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct RotStatus {
    pub version: Version,
    pub min_version: Version,
    /// Max buffer size for receiving requests on the RoT
    pub request_buf_size: u16,
    /// Max buffer size for sending responses on the RoT
    pub response_buf_size: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub struct SpStatus {
    pub version: Version,
    pub min_version: Version,
    /// Max buffer size for sending requests on the SP
    pub request_buf_size: u16,
    /// Max buffer size for receiving responses on the SP
    pub response_buf_size: u16,
}

/// RoT boot info
#[derive(Debug, Clone, Serialize, Deserialize, SerializedSize)]
pub enum RotState {
    // We expect to evolve this in short order to include caboose info, boot
    // selection for the new stage0, cfpa, etc...
    V1 {
        state: RotBootState,

        /// CRC32 of the LPC55 boot ROM contents.
        ///
        /// The LPC55 does not have machine readable version information for
        /// its boot ROM contents and there are known issues with old boot
        /// ROMs.
        bootrom_crc32: u32,
    },
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
pub struct SprotIoStats {
    pub rot: RotIoStats,
    pub sp: SpIoStats,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
