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
use userlib::{sys_send, FromPrimitive};

/// Sprot protocol specific errors
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum SprotError {
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

/// Switch Default Image payload
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub struct SwitchDefaultImageHeader {
    pub slot: SlotId,
    pub duration: SwitchDuration,
}

/// Protocol version
/// This is the first byte of any Sprot request or response
#[derive(
    Copy, Clone, Eq, PartialEq, Deserialize, Serialize, SerializedSize,
)]
#[repr(u8)]
pub enum Protocol {
    /// Indicates that no message is present.
    Ignore = 0x00,
    /// The first sprot format with hand-rolled serialization.
    V1 = 0x01,
    /// The second format, using hubpack
    V2 = 0x02,
    /// Never to be seen. Queued by RoT when not ready.
    ///
    /// SPI has no flow control, i.e. the RoT has no busy state indication
    /// visible to the SP.
    /// In future designs, we could consider adding a "ROT_READY" in addition
    /// to ROT_IRQ or use an interconect that does have flow control.
    ///
    /// The RoT places 0xB2 (Busy -> "B" "Z" -> 0xB2) in its transmit FIFO when
    /// it is not prepared to service SPI IO. If the SP ever clocks out
    /// data before the RoT is ready, the SP will read 0xB2.
    /// This code should never be seen on the SPI bus. If seen as the first
    /// byte in a message or on the SPI bus with a logic analyzer, that needs
    /// to be investigated and fixed.
    ///
    /// It may mean that the two parties are out of phase, some RoT task
    /// is hampering the sprot task from meeting its realtime requirements,
    /// or there is some other bug.
    Busy = 0xb2,
}

/// A sprot request from the SP to the RoT
#[derive(Serialize, Deserialize, SerializedSize)]
pub struct Request {
    protocol: Protocol,
    body: ReqBody,
    crc: u16,
}

impl Request {
    /// Create a `Request` with `Protocol::V2` header, calculate a CRC16 over
    // the `length`, `protocol` and `body` fields, then serialize it into `buf` with
    // hubpack, returning the serialized size.
    pub fn pack(body: ReqBody, buf: &mut [u8]) -> Result<size, hubpack::Error> {
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
    pub fn unpack(buf: &[u8]) -> Result<Request, SprotError> {
        let protocol = Protocol::V2;
        if buf[0] != protocol {
            return Err(SprotError::UnsupportedProtocol);
        }
        let crc_start = buf.len() - 2;
        let (body, _) = hubpack::deserialize(&buf[1..crc_start])?;
        let (crc, _) = hubpack::deserialize(&buf[crc_start..][..2])?;
        let computed = CRC16.checksum(&self.buf[..crc_start]);
        if computed == crc {
            Ok(Request {
                protocol,
                body,
                crc,
            })
        } else {
            Err(SprotError::InvalidCrc)
        }
    }
}

/// The body of a sprot request
#[derive(Clone, Serialize, Deserialize, SerializedSize)]
pub enum ReqBody {
    Error(ReqError),
    Status,
    IoStats,
    Sprockets(SprocketsReq),
    Update(UpdateReq),
    Dump(DumpReq),
}

/// An error returned from a sprot request
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, SerializedSize)]
pub enum ReqError {
    SprotError(SprotError),
    SpiError(SpiError),
    UpdateError(UpdateError),
}

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
const CRC_SIZE: usize = <u16 as SerializedSize>::MAX_SIZE;
// XXX ROT FIFO size should be discovered.
pub const ROT_FIFO_SIZE: usize = 8;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
