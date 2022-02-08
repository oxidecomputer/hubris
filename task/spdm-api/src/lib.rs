// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the SPDM Server

#![no_std]

use userlib::*;

/// Errors that can be produced from the SPDM server API.
///
#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u32)]
pub enum SpdmError {
    /// A previous message is not yet processed.
    MessageAlreadyExists = 1,
    /// There is no message ready to be delivered.
    NoMessageAvailable = 2,
    /// The sent message is too short.
    ShortMessage = 3,
    /// The receive buffer is too small for the available message.
    SinkTooSmall = 4,
    /// The message being sent exceeds the internal buffer size.
    SourceTooLarge = 5,
}

impl From<u32> for SpdmError {
    fn from(x: u32) -> Self {
        match x {
            1 => SpdmError::MessageAlreadyExists,
            2 => SpdmError::NoMessageAvailable,
            3 => SpdmError::ShortMessage,
            4 => SpdmError::SinkTooSmall,
            5 => SpdmError::SourceTooLarge,
            _ => panic!(),
        }
    }
}

impl From<SpdmError> for u16 {
    fn from(rc: SpdmError) -> Self {
        rc as u16
    }
}

impl From<SpdmError> for u32 {
    fn from(rc: SpdmError) -> Self {
        rc as u32
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
