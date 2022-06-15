// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]
extern crate memoffset;

use crc::{Crc, CRC_16_XMODEM};
use derive_idol_err::IdolError;
// Chained if let statements are almost here.
use if_chain::if_chain;
use userlib::{sys_send, FromPrimitive};
use zerocopy::*;

// TODO: Audit that each MsgError is used and has some reasonable action.
// While a diverse set of error codes may be useful for debugging it
// clutters code that just has to deal with the error.
// then consider adding a function that translates an error code
// into the desired action, e.g. InvalidCrc and FlowError should both
// result in a retry on the SP side and an ErrorRsp on the RoT side.

#[derive(Copy, Clone, FromPrimitive, PartialEq, Eq, IdolError)]
pub enum MsgError {
    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadTransferSize = 1,

    /// Server restarted
    // ServerRestarted = 2,

    /// CRC check failed.
    InvalidCrc = 3,

    /// FIFO overflow/underflow
    FlowError = 4,

    /// Unsupported protocol version
    UnsupportedProtocol = 5,

    /// Unknown message
    BadMessageType = 6,

    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadMessageLength = 7,

    /// Error from Spi
    SpiServerError = 8,

    /// Message is too large
    Oversize = 9,

    /// Tx buffer is unexpectedly not Idle.
    TxNotIdle = 10,

    CannotAssertCSn = 11,

    RotNotReady = 12,

    RspTimeout = 13,

    BadResponse = 14,

    RotBusy = 15,

    /// Feature is not implemented
    NotImplemented = 16,

    /// An error code reserved for the SP was used by the Rot
    NonRotError = 17,

    /// A message with version = 0 was received unexpectedly.
    EmptyMessage = 18,
    /// Unknown Errors are mapped to 0xff
    Unknown = 0xff,
}

/// Protocol version
pub const VERSION_IGNORE: u8 = 0; // To be ignored
pub const VERSION_1: u8 = 1; // Supported message format

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
pub const VERSION_BUSY: u8 = 0xB2;

/// SPI Message types will allow for multiplexing and forward compatibility.
#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive, Unaligned, AsBytes)]
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
    /// Reserved value.
    Unknown = 0xff,
}

#[derive(Copy, Clone, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct MsgHeader {
    version: u8,
    msgtype: u8,
    length: U16<byteorder::LittleEndian>,
}

#[derive(Copy, Clone, Unaligned, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct SpRotReturn {
    pub length: u32,
    pub msgtype: u8,
}

impl SpRotReturn {
    pub fn msgtype(&self) -> MsgType {
        MsgType::from_u8(self.msgtype).unwrap_or(MsgType::Unknown)
    }

    pub fn length(&self) -> usize {
        self.length as usize
    }
}

type Crc16 = U16<byteorder::LittleEndian>;
pub const HEADER_SIZE: usize = core::mem::size_of::<MsgHeader>();
pub const PAYLOAD_SIZE_MAX: usize = 1024;
pub const CRC_SIZE: usize = core::mem::size_of::<Crc16>();
pub const REQ_BUF_SIZE: usize = HEADER_SIZE + PAYLOAD_SIZE_MAX + CRC_SIZE;
pub const RSP_BUF_SIZE: usize = HEADER_SIZE + PAYLOAD_SIZE_MAX + CRC_SIZE;

#[derive(Copy, Clone, Unaligned, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct Msg {
    header: MsgHeader,
    payload: [u8; PAYLOAD_SIZE_MAX],
    _padding: [u8; CRC_SIZE], // Ensure there is always room for the CRC
}

impl From<&[u8]> for MsgHeader {
    fn from(data: &[u8]) -> Self {
        let version = if let Some(bytes) = data.get(0..=0) {
            bytes[0]
        } else {
            0
        };
        let msgtype = if let Some(bytes) = data.get(1..=1) {
            bytes[0]
        } else {
            0
        };
        let length = if let Some(bytes) = data.get(2..=3) {
            LittleEndian::read_u16(bytes)
        } else {
            0
        };
        Self {
            version,
            msgtype,
            length: length.into(),
        }
    }
}

pub enum EnqueueBuf<'a> {
    /// Copy a slice into the transmit buffer.
    Copy(&'a [u8]),
    /// Use the already extant data in the transmit buffer.
    TxBuf(usize),
    /// Message header only. There is no payload.
    Empty,
}

impl Msg {
    pub fn new() -> Self {
        Msg {
            header: MsgHeader {
                version: VERSION_IGNORE,
                msgtype: MsgType::Invalid as u8,
                length: 0.into(),
            },
            payload: [0u8; PAYLOAD_SIZE_MAX],
            // Make room for a trailing CRC given a maximum payload.
            _padding: [0u8; CRC_SIZE],
        }
    }

    // accessors for reading/writing bytes that compose a message.

    /// Write access to the entire message buffer.
    pub fn buf_mut(&mut self) -> &mut [u8] {
        self.as_bytes_mut()
    }

    /// Read access to the entire message buffer.
    pub fn buf(&mut self) -> &[u8] {
        self.as_bytes()
    }

    /// Write access to header bytes.
    pub fn header_buf_mut(&mut self) -> &mut [u8] {
        self.header.as_bytes_mut()
    }

    /// Read header bytes.
    pub fn header_buf(&self) -> &[u8] {
        self.header.as_bytes()
    }

    /// Read/Write access to payload bytes, not validated.
    pub fn payload_buf_mut(&mut self) -> &mut [u8; PAYLOAD_SIZE_MAX] {
        &mut self.payload
    }

    pub fn set_payload_len(&mut self, len: usize) {
        if len > self.payload.len() {
            panic!(); // XXX Remove all panics.
        }
        self.header.length.set(len as u16);
    }

    pub fn unvalidated_payload_len(&self) -> u16 {
        self.header.length.get()
    }

    pub fn payload_len(&self) -> Option<usize> {
        if !self.is_supported_version() {
            return None;
        }
        let end = self.unvalidated_payload_len() as usize;
        if end <= self.payload.len() {
            Some(end)
        } else {
            None
        }
    }

    /// Read payload bytes as specified by the message header.
    pub fn payload_buf(&self) -> Option<&[u8]> {
        if let Some(buf) = self
            .payload
            .get(0..(self.unvalidated_payload_len() as usize))
        {
            Some(buf)
        } else {
            None
        }
    }

    pub fn set_version(&mut self) {
        self.header.version = VERSION_1;
    }

    pub fn is_supported_version(&self) -> bool {
        self.header.version == VERSION_1
    }

    pub fn is_ignored_version(&self) -> bool {
        self.header.version == VERSION_IGNORE
    }

    pub fn is_busy_version(&self) -> bool {
        self.header.version == VERSION_BUSY
    }

    pub fn set_msgtype(&mut self, msgtype: MsgType) {
        self.header.msgtype = msgtype as u8;
    }

    pub fn msgtype(&self) -> MsgType {
        MsgType::from_u8(self.header.msgtype).unwrap_or(MsgType::Unknown)
    }

    pub fn init(&mut self, msgtype: MsgType, len: usize) {
        self.set_version();
        self.set_msgtype(msgtype);
        self.set_payload_len(len);
    }

    /// Compute the CRC16 of the message header and payload.
    fn crc(self) -> Option<u16> {
        pub const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_XMODEM);
        let mut digest = CRC16.digest();
        if_chain! {
            if let Some(bytes) = self.bytes();
            if let Some(bytes) = bytes.get(0..bytes.len()-CRC_SIZE);
            then {
                digest.update(bytes);
                Some(digest.finalize())
            } else {
                None
            }
        }
    }

    /// Compute the CRC16 of the message header and payload and write
    /// it to the end of the header defined payload.
    pub fn set_crc(&mut self) -> bool {
        if let Some(computed_crc) = self.crc() {
            let len = self.len();
            if let Some(msg_crc) =
                self.as_bytes_mut().get_mut(len - CRC_SIZE..len)
            {
                LittleEndian::write_u16(msg_crc, computed_crc);
                return true;
            }
        }
        false
    }

    /// Compute the message CRC and compare to the CRC stored in the message.
    pub fn is_crc_valid(self) -> bool {
        if let Some(size) = self.payload_len() {
            if let Some(bytes) = self
                .as_bytes()
                .get(HEADER_SIZE + size..HEADER_SIZE + CRC_SIZE + size)
            {
                let crc = LittleEndian::read_u16(bytes);
                if let Some(computed_crc) = self.crc() {
                    return computed_crc == crc;
                }
            }
        }
        false
    }

    // Note that the computed length is not validated here.
    // There is always a header, even if invalid. So, there can never
    // be an empty message.
    pub fn len(&self) -> usize {
        HEADER_SIZE + (self.unvalidated_payload_len() as usize) + CRC_SIZE
    }

    pub fn is_empty(&self) -> bool {
        !self.is_supported_version() || self.payload_len().is_none()
    }

    /// Access all bytes of a validated message.
    pub fn bytes(&self) -> Option<&[u8]> {
        if self.is_empty() {
            None
        } else {
            self.as_bytes().get(0..self.len())
        }
    }

    // Enqueue a message for Tx to the SP.
    // The message data may be:
    //   - zero-length (header only),
    //   - copied from an external buffer, or
    //   - the Tx buffer may already contain the data
    //     so only the length needs to be provided.
    pub fn enqueue(
        &mut self,
        msgtype: MsgType,
        buf: EnqueueBuf<'_>,
    ) -> Result<usize, MsgError> {
        let tx_payload_len = match buf {
            EnqueueBuf::Copy(buf) => buf.len(),
            EnqueueBuf::TxBuf(len) => len,
            EnqueueBuf::Empty => 0,
        };
        if tx_payload_len > self.payload_buf_mut().len() {
            // The payload content offered is bigger than the available buffer.
            return Err(MsgError::Oversize);
        }
        self.set_version();
        self.set_msgtype(msgtype);
        self.set_payload_len(tx_payload_len);
        if_chain! {
            if let EnqueueBuf::Copy(input) = buf;
            if let Some(dest) = self.payload_buf_mut().get_mut(0..tx_payload_len);
            if let Some(src) = input.get(0..tx_payload_len);
            then {
                dest.clone_from_slice(src);
            }
        }
        self.set_crc();

        // Zero the tail to to allow easy output of trailing zeros.
        // Note: Zeroing the tail is proper on the RoT, but maybe not useful
        // on the SP side.
        if let Some(remainder) = self
            .buf_mut()
            .get_mut(HEADER_SIZE + tx_payload_len + CRC_SIZE..)
        {
            remainder.fill(0x00);
        }
        Ok(HEADER_SIZE + tx_payload_len + CRC_SIZE)
    }

    // /// Set all header and payload bytes to zero.
    // pub fn enqueue_zeros(&mut self) {
    //    self.buf_mut().fill(0);
    //}
}

impl Default for Msg {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Unaligned, FromBytes, AsBytes, Eq, PartialEq)]
#[repr(C, packed)]
pub struct SpRotPulseStatus {
    pub rot_irq_begin: u8,
    pub rot_irq_end: u8,
}

#[derive(Copy, Clone, Unaligned, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct SpRotSinkStatus {
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
#[derive(Copy, Clone, Unaligned, FromBytes, AsBytes)]
#[repr(C, packed)]
pub struct Status {
    /// All supported versions 'v' from 1 to 32 as a mask of (1 << v-1)
    pub supported: u32,

    /// CRC32 of the LPC55 boot ROM contents.
    /// The LPC55 does not have machine readable version information for
    /// its boot ROM contents and there are known issues with old boot ROMs.
    pub bootrom_crc32: u32,

    /// The currently running firmware version.
    pub fwversion: u32,

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

    /// Number of times the received message could not be handled completly
    pub handler_error: u32,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
