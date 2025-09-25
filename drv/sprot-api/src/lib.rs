// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]
#![deny(elided_lifetimes_in_paths)]
extern crate memoffset;

mod error;
use attest_api::{AttestError, HashAlgorithm};
use drv_caboose::CabooseError;
use dumper_api::DumperError;
pub use error::{
    AttestOrSprotError, CabooseOrSprotError, DumpOrSprotError,
    RawCabooseOrSprotError, SprocketsError, SprotError, SprotProtocolError,
    StateError, StateOrSprotError, WatchdogError,
};

use crc::{Crc, CRC_16_XMODEM};
use derive_more::From;
pub use drv_lpc55_update_api::{
    Fwid, HandoffDataLoadError, ImageError, ImageVersion, RawCabooseError,
    RotBootInfo, RotBootInfoV2, RotBootState, RotBootStateV2, RotComponent,
    RotImageDetails, RotPage, RotSlot, SlotId, SwitchDuration, UpdateTarget,
    VersionedRotBootInfo,
};
pub use drv_update_api::UpdateError;
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
pub const CURRENT_VERSION: Version = Version(6);

/// We allow room in the buffer for message evolution
pub const REQUEST_BUF_SIZE: usize = 1024;
const_assert!(
    REQUEST_BUF_SIZE
        > Header::MAX_SIZE + ReqBody::MAX_SIZE + MAX_BLOB_SIZE + CRC_SIZE
);

/// We allow room in the buffer for message evolution
pub const RESPONSE_BUF_SIZE: usize = 1024;
const_assert!(
    RESPONSE_BUF_SIZE
        > Header::MAX_SIZE + RspBody::MAX_SIZE + MAX_BLOB_SIZE + CRC_SIZE
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
        Self::pack_with_cb(body, buf, |buf| {
            // Copy the blob into the buffer after the serialized body
            blob.read_range(0..blob.len(), buf)
                .map_err(|_| SprotProtocolError::TaskRestarted)?;
            Ok(blob.len())
        })
    }

    /// Serialize a `Header` followed by a `ReqBody` or `RspBody`, copy a blob
    /// into `buf` after the serialized body,  compute a CRC, serialize the
    /// CRC, and return the total size of the serialized request.
    pub fn pack_with_cb<F, E>(
        body: &T,
        buf: &mut [u8; N],
        mut cb: F,
    ) -> Result<usize, E>
    where
        F: FnMut(&mut [u8]) -> Result<usize, E>,
    {
        // Serialize `body`
        let mut size = hubpack::serialize(&mut buf[Header::MAX_SIZE..], body)
            .unwrap_lite();

        size += cb(&mut buf[Header::MAX_SIZE + size..])?;

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

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CfpaState {
    /// The CFPA page used by the ROM
    Active,
    /// The CFPA that will be applied on the next update
    Pending,
    /// The CFPA region that is neither pending or active
    Alternate,
}

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum PageReq {
    Page(RotPage),
}

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
    // Added in sprot protocol version 3
    Caboose(CabooseReq),
    Attest(AttestReq),
    // Added in sprot protocol version 4
    RotPage { page: RotPage },
    // Added in sprot protocol version 5
    Swd(SwdReq),
    // Added in sprot protocol version 6
    State(StateReq),
}

// Added in sprot protocol version 5
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum SwdReq {
    EnableSpSlotWatchdog {
        time_ms: u32,
    },
    DisableSpSlotWatchdog,

    /// Checks whether the SP slot watchdog is supported
    ///
    /// In practice, this calls `drv_lpc55_swd::ServerImpl::setup` to make sure
    /// that there's no debugger attached that would prevent us from talking to
    /// the SP.
    SpSlotWatchdogSupported,
}

// Added in sprot protocol version 6
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum StateReq {
    /// Checks the RoT's lifecycle state, per RFD 286
    LifecycleState,
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
    // Added in sprot protocol version 3
    BootInfo,
    VersionedBootInfo {
        version: u8,
    },
    ComponentPrep {
        component: RotComponent,
        slot: SlotId,
    },
    ComponentSwitchDefaultImage {
        component: RotComponent,
        slot: SlotId,
        duration: SwitchDuration,
    },
    ComponentSwitchCancelPending {
        component: RotComponent,
        slot: SlotId,
        duration: SwitchDuration,
    },
}

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CabooseReq {
    /// Size of the caboose for Hubris slot A or B
    Size { slot: SlotId },
    /// Read caboose of Hubris slot A or B
    Read { slot: SlotId, start: u32, size: u32 },
    /// Size of the caboose of a component's slot A or B
    ComponentSize {
        component: RotComponent,
        slot: SlotId,
    },
    /// Read caboose of component's slot A or B
    ComponentRead {
        component: RotComponent,
        slot: SlotId,
        start: u32,
        size: u32,
    },
}

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum AttestReq {
    CertChainLen,
    CertLen(u32),
    Cert { index: u32, offset: u32, size: u32 },
    Record { algorithm: HashAlgorithm },
    Log { offset: u32, size: u32 },
    LogLen,
    Attest { nonce_size: u32, write_size: u32 },
    AttestLen,
    // Added in protocol version 6
    TqCertChainLen,
    TqCertLen(u32),
    TqCert { index: u32, offset: u32, size: u32 },
    TqSign { write_size: u32 },
    TqSignLen,
}

/// A response used for RoT updates
#[derive(Clone, Serialize, Deserialize, SerializedSize, From)]
pub enum UpdateRsp {
    BlockSize(u32),
    // Added in sprot protocol version 3
    BootInfo(RotBootInfo),
    VersionedBootInfo(VersionedRotBootInfo),
}

/// A response used for caboose requests
//
// Added in sprot protocol version 3
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum CabooseRsp {
    Size(u32),
    Read,
    ComponentSize(u32),
    ComponentRead,
}

#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum AttestRsp {
    CertChainLen(u32),
    CertLen(u32),
    Cert,
    Record,
    Log,
    LogLen(u32),
    Attest,
    AttestLen(u32),
    // Added in version 6
    TqCertChainLen(u32),
    TqCertLen(u32),
    TqCert,
    TqSign,
    TqSignLen(u32),
}

/// The body of a sprot response.
///
/// See [`Msg`] for details about versioning and message evolution.
#[derive(Clone, Serialize, Deserialize, SerializedSize, From)]
#[allow(clippy::large_enum_variant)]
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

    // Added in sprot protocol version 3
    Caboose(Result<CabooseRsp, RawCabooseError>),

    Attest(Result<AttestRsp, AttestError>),

    Page(Result<RotPageRsp, UpdateError>),

    // Added in sprot protocol version 6
    State(Result<StateRsp, StateError>),
}

/// A response for reading a ROT page
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub enum RotPageRsp {
    RotPage,
}

/// Life-cycle state, as defined in RFD 286
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub enum LifecycleState {
    /// Any state in which the CMPA is unlocked counts as unprogrammed
    Unprogrammed,

    /// At least one of the release trust anchors is valid, and both of the
    /// development trust anchors are invalid
    Release,

    /// At least one of the development trust anchors is valid, and both of the
    /// release trust anchors are revoked
    Development,

    /// All four trust anchors are revoked
    EndOfLife,
}

#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub enum StateRsp {
    LifecycleState(LifecycleState),
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
    V1 {
        state: RotBootState,

        /// CRC32 of the LPC55 boot ROM contents.
        ///
        /// The LPC55 does not have machine readable version information for
        /// its boot ROM contents and there are known issues with old boot
        /// ROMs.
        bootrom_crc32: u32,
    },
    V2 {
        state: RotBootStateV2,
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

    /// Number of times when the RoT thinks its receiving for a request, while
    /// the SP thinks it is receiving a response, or the RoT thinks it is
    /// sending a response while the SP thinks it is sending a request.
    pub desynchronized: u32,
}

/// Stats from the SP side of sprot
///
/// All of the counters will wrap around.
#[derive(
    Default, Copy, Clone, PartialEq, Serialize, Deserialize, SerializedSize,
)]
pub struct SpIoStats {
    /// Number of messages sent successfully
    pub tx_sent: u32,

    /// Number of messages that failed to be sent
    pub tx_errors: u32,

    /// Number of messages received successfully
    pub rx_received: u32,

    /// Number of error replies received
    pub rx_errors: u32,

    /// Number of invalid messages received. They don't parse properly.
    pub rx_invalid: u32,

    /// Total Number of retries issued
    pub retries: u32,

    /// Number of times the SP pulsed CSn
    pub csn_pulses: u32,

    /// Number of times pulsing CSn failed.
    pub csn_pulse_failures: u32,

    /// Number of timeouts, while waiting for a reply
    pub timeouts: u32,

    /// Number of times the RoT has reported that it was desynchronized
    pub desynchronized: u32,
}

/// Sprot related stats
#[derive(Default, Clone, Serialize, Deserialize, SerializedSize)]
pub struct SprotIoStats {
    pub rot: RotIoStats,
    pub sp: SpIoStats,
}

impl SpRot {
    pub fn read_caboose_value(
        &self,
        component: RotComponent,
        slot_id: SlotId,
        key: [u8; 4],
        buf: &mut [u8],
    ) -> Result<u32, CabooseOrSprotError> {
        let reader = RotCabooseReader::new(component, slot_id, self)?;
        let len = reader.get(key, buf)?;
        Ok(len)
    }
}

#[derive(Copy, Clone)]
struct RotCabooseReader<'a> {
    sprot: &'a SpRot,
    size: u32,
    component: RotComponent,
    slot: SlotId,
}

impl<'a> RotCabooseReader<'a> {
    fn new(
        component: RotComponent,
        slot: SlotId,
        sprot: &'a SpRot,
    ) -> Result<Self, CabooseOrSprotError> {
        let size = match component {
            // Use old API for backward compatibility until
            // it can be deprecated with anti-rollback/epoch.
            RotComponent::Hubris => sprot.caboose_size(slot)?,
            _ => sprot.component_caboose_size(component, slot)?,
        };
        Ok(Self {
            size,
            component,
            slot,
            sprot,
        })
    }

    pub fn get(
        &self,
        key: [u8; 4],
        out: &mut [u8],
    ) -> Result<u32, CabooseOrSprotError> {
        // This is similar to the implementation in drv_caboose::CabooseReader,
        // but goes through the SpRot IPC bridge to request data over SPI.  In
        // addition, it copies the found value in this function, instead of
        // returning a `&'static [u8]`; returning a slice would be meaningless
        // because the value is not in local memory.
        let mut reader = tlvc::TlvcReader::begin(self).map_err(|_| {
            CabooseOrSprotError::Caboose(CabooseError::TlvcReaderBeginFailed)
        })?;
        loop {
            match reader.next() {
                Ok(Some(chunk)) => {
                    if chunk.header().tag == key {
                        let mut tmp = [0u8; 32];
                        if chunk.check_body_checksum(&mut tmp).is_err() {
                            return Err(CabooseOrSprotError::Caboose(
                                CabooseError::BadChecksum,
                            ));
                        }
                        let data_len = chunk.header().len.get();

                        if data_len as usize > out.len() {
                            return Err(CabooseOrSprotError::Sprot(
                                SprotError::Protocol(
                                    SprotProtocolError::BadMessageLength,
                                ),
                            ));
                        }

                        chunk
                            .read_exact(0, &mut out[..data_len as usize])
                            .map_err(|_| {
                                CabooseOrSprotError::Caboose(
                                    CabooseError::RawReadFailed,
                                )
                            })?;
                        return Ok(data_len);
                    }
                }
                Err(e) => match e {
                    tlvc::TlvcReadError::Truncated => {
                        return Err(CabooseOrSprotError::Caboose(
                            CabooseError::NoSuchTag,
                        ))
                    }
                    tlvc::TlvcReadError::HeaderCorrupt { .. }
                    | tlvc::TlvcReadError::BodyCorrupt { .. } => {
                        return Err(CabooseOrSprotError::Caboose(
                            CabooseError::BadChecksum,
                        ))
                    }
                    tlvc::TlvcReadError::User(e) => break Err(e.into()),
                },
                Ok(None) => {
                    return Err(CabooseOrSprotError::Caboose(
                        CabooseError::NoSuchTag,
                    ))
                }
            }
        }
    }
}

impl tlvc::TlvcRead for &RotCabooseReader<'_> {
    type Error = RawCabooseOrSprotError;
    fn extent(&self) -> Result<u64, tlvc::TlvcReadError<Self::Error>> {
        Ok(self.size as u64)
    }

    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), tlvc::TlvcReadError<Self::Error>> {
        let offset = offset
            .try_into()
            .map_err(|_| tlvc::TlvcReadError::Truncated)?;
        match self.component {
            RotComponent::Hubris => self
                .sprot
                .read_caboose_region(offset, self.slot, dest)
                .map_err(tlvc::TlvcReadError::User),
            _ => self
                .sprot
                .component_read_caboose_region(
                    offset,
                    self.component,
                    self.slot,
                    dest,
                )
                .map_err(tlvc::TlvcReadError::User),
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
