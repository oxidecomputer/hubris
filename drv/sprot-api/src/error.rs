// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Errors for the sprot API

use derive_more::From;
use drv_spi_api::SpiError;
use drv_update_api::UpdateError;
use dumper_api::DumperError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::FromPrimitive;

use gateway_messages::{
    RotError, SpError, SprocketsError as GwSprocketsErr,
    SprotProtocolError as GwSprotProtocolError,
};
use idol_runtime::RequestError;

/// An error returned from a sprot request
#[derive(
    Debug, Copy, Clone, Serialize, Deserialize, SerializedSize, From, PartialEq,
)]
pub enum SprotError {
    Protocol(SprotProtocolError),
    Spi(SpiError),
    Update(UpdateError),
    Sprockets(SprocketsError),
}

impl From<SprotError> for SpError {
    fn from(value: SprotError) -> Self {
        match value {
            SprotError::Protocol(e) => Self::Sprot(e.into()),
            SprotError::Spi(e) => Self::Spi(e.into()),
            SprotError::Update(e) => Self::Update(e.into()),
            SprotError::Sprockets(e) => Self::Sprockets(e.into()),
        }
    }
}

impl From<SprotError> for RotError {
    fn from(value: SprotError) -> Self {
        match value {
            SprotError::Protocol(e) => Self::Sprot(e.into()),
            SprotError::Spi(e) => Self::Spi(e.into()),
            SprotError::Update(e) => Self::Update(e.into()),
            SprotError::Sprockets(e) => Self::Sprockets(e.into()),
        }
    }
}

impl From<idol_runtime::ServerDeath> for SprotError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        SprotError::Protocol(SprotProtocolError::TaskRestarted)
    }
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
    // The RoT has not de-asserted ROT_IRQ
    RotIrqRemainsAsserted,
    // An unexpected response was received.
    // This should basically be impossible. We only include it so we can
    // return this error when unpacking a RspBody in idol calls.
    UnexpectedResponse,

    // Failed to load update status
    BadUpdateStatus,

    // Used for mapping From<idol_runtime::ServerDeath>
    TaskRestarted,
}

impl From<SprotProtocolError> for GwSprotProtocolError {
    fn from(value: SprotProtocolError) -> Self {
        match value {
            SprotProtocolError::InvalidCrc => Self::InvalidCrc,
            SprotProtocolError::FlowError => Self::FlowError,
            SprotProtocolError::UnsupportedProtocol => {
                Self::UnsupportedProtocol
            }
            SprotProtocolError::BadMessageType => Self::BadMessageType,
            SprotProtocolError::BadMessageLength => Self::BadMessageLength,
            SprotProtocolError::CannotAssertCSn => Self::CannotAssertCSn,
            SprotProtocolError::Timeout => Self::Timeout,
            SprotProtocolError::Deserialization => Self::Deserialization,
            SprotProtocolError::RotIrqRemainsAsserted => {
                Self::RotIrqRemainsAsserted
            }
            SprotProtocolError::UnexpectedResponse => Self::UnexpectedResponse,
            SprotProtocolError::BadUpdateStatus => Self::BadUpdateStatus,
            SprotProtocolError::TaskRestarted => Self::TaskRestarted,
        }
    }
}

impl From<SprotProtocolError> for RequestError<SprotError> {
    fn from(err: SprotProtocolError) -> Self {
        SprotError::from(err).into()
    }
}

impl From<hubpack::Error> for SprotError {
    fn from(_: hubpack::Error) -> Self {
        SprotProtocolError::Deserialization.into()
    }
}

impl From<hubpack::Error> for SprotProtocolError {
    fn from(_: hubpack::Error) -> Self {
        SprotProtocolError::Deserialization
    }
}

impl SprotError {
    pub fn is_recoverable(&self) -> bool {
        match *self {
            SprotError::Protocol(err) => {
                use SprotProtocolError::*;
                match err {
                    InvalidCrc | FlowError | Timeout | TaskRestarted
                    | Deserialization => true,
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

// There are currently no other exposed sprockets errors,
// and sprockets isn't in use yet. This is just a place holder.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum SprocketsError {
    BadEncoding,
    UnsupportedVersion,
}

impl From<SprocketsError> for GwSprocketsErr {
    fn from(value: SprocketsError) -> Self {
        match value {
            SprocketsError::BadEncoding => Self::BadEncoding,
            SprocketsError::UnsupportedVersion => Self::UnsupportedVersion,
        }
    }
}

impl From<SprotError> for RequestError<DumpOrSprotError> {
    fn from(err: SprotError) -> Self {
        DumpOrSprotError::from(err).into()
    }
}

#[derive(Copy, Clone, Debug, From, Deserialize, Serialize, SerializedSize)]
pub enum DumpOrSprotError {
    Dump(DumperError),
    Sprot(SprotError),
}

/// A new type to prevent orphan rule problems on the conversion below
pub struct DumperReturnCode(pub u32);

impl From<DumperReturnCode> for Result<(), RequestError<DumpOrSprotError>> {
    fn from(value: DumperReturnCode) -> Self {
        if value.0 == 0 {
            Ok(())
        } else {
            let err = DumperError::from_u32(value.0)
                .unwrap_or(DumperError::UnknownFailureViaSprot);
            Err(RequestError::Runtime(err.into()))
        }
    }
}
