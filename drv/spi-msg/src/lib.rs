// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SP to RoT messages over SPI.

#![no_std]

use derive_idol_err::IdolError;
use zerocopy::{AsBytes, ByteSliceMut, FromBytes, LayoutVerified, Unaligned};
//use userlib::*;
use hubpack::SerializedSize;
use sprockets_common::msgs::{RotRequestV1, RotResponseV1};
use userlib::{sys_send, FromPrimitive};

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, IdolError)]
pub enum MsgError {
    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadTransferSize = 1,

    /// Server restarted
    ServerRestarted = 2,

    /// FIFO overflow/underflow
    FlowError = 3,

    /// Unsupported protocol version
    UnsupportedProtocol = 4,

    /// Unknown message
    BadMessageType = 5,

    /// Transfer size is outside of maximum and minimum lenghts for message type.
    BadMessageLength = 6,

    /// Error from Spi
    SpiServerError = 7,
}

/// Protocol version
pub const SPI_MSG_IGNORE: u8 = 0; // To be ignored
pub const SPI_MSG_VERSION: u8 = 1; // Supported message format

/// SPI Message types will allow for multiplexing and forward compatibility.
#[derive(
    Copy, Clone, PartialEq, Eq, Debug, userlib::FromPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum MsgType {
    Invalid = 0,
    Error = 1,
    Echo = 2,
    EchoReturn = 3,
    Status = 4,
    Sprockets = 5,
    Unknown = 0xff,
}

impl From<u8> for MsgType {
    fn from(msgtype: u8) -> Self {
        match msgtype {
            0 => MsgType::Invalid,
            1 => MsgType::Error,
            2 => MsgType::Echo,
            3 => MsgType::EchoReturn,
            4 => MsgType::Status,
            5 => MsgType::Sprockets,
            _ => MsgType::Unknown,
        }
    }
}

impl From<u32> for MsgType {
    fn from(msgtype: u32) -> Self {
        match msgtype {
            0 => MsgType::Invalid,
            1 => MsgType::Error,
            2 => MsgType::Echo,
            3 => MsgType::EchoReturn,
            4 => MsgType::Status,
            5 => MsgType::Sprockets,
            _ => MsgType::Unknown,
        }
    }
}

#[derive(FromBytes, AsBytes, Unaligned, Debug)]
#[repr(C)]
pub struct MsgHeader {
    version: u8,
    len_lsb: u8,
    len_msb: u8,
    msgtype: u8,
}
pub const SPI_HEADER_SIZE: usize = core::mem::size_of::<MsgHeader>();
pub const MAX_SPI_MSG_PAYLOAD_SIZE: usize = 512;
pub const SPI_REQ_BUF_SIZE: usize = SPI_HEADER_SIZE + RotRequestV1::MAX_SIZE;
pub const SPI_RSP_BUF_SIZE: usize = SPI_HEADER_SIZE + RotResponseV1::MAX_SIZE;

pub struct Msg<B> {
    header: LayoutVerified<B, MsgHeader>,
    body: B,
}

impl<'a, B: ByteSliceMut> Msg<B> {
    pub fn parse(bytes: B) -> Option<Msg<B>> {
        let (header, body) = LayoutVerified::new_unaligned_from_prefix(bytes)?;
        Some(Msg { header, body })
    }
    pub fn is_supported_version(&self) -> bool {
        self.header.version == SPI_MSG_VERSION
    }

    // There is always a header, even if invalid. So, there can never
    // be an empty message.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        SPI_HEADER_SIZE + self.payload_len()
    }

    pub fn payload_len(&self) -> usize {
        ((self.header.len_msb as usize) << 8) | (self.header.len_lsb as usize)
    }

    pub fn msgtype(&self) -> MsgType {
        self.header.msgtype.into()
    }

    pub fn set_version(&mut self) {
        self.header.version = SPI_MSG_VERSION;
    }

    pub fn set_len(&mut self, len: usize) {
        self.header.len_lsb = (len & 0xff) as u8;
        self.header.len_msb = (len >> 8) as u8;
    }

    pub fn set_msgtype(&mut self, msgtype: MsgType) {
        self.header.msgtype = msgtype as u8;
    }

    pub fn payload_buf(&'a mut self) -> &'a mut [u8] {
        &mut self.body[..]
    }

    pub fn payload_get(&'a self) -> Result<&'a [u8], MsgError> {
        if !self.is_supported_version() {
            return Err(MsgError::UnsupportedProtocol);
        }
        if self.payload_len() <= self.body.len() {
            Ok(&self.body[..self.payload_len()])
        } else {
            Err(MsgError::BadMessageLength)
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
