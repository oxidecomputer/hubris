// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Errors for the sprot API

use attest_api::AttestError;
use derive_more::From;
use drv_caboose::CabooseError;
use drv_lpc55_update_api::RawCabooseError;
use drv_update_api::UpdateError;
use dumper_api::DumperError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};

use attest_data::messages::RecvSprotError as AttestDataSprotError;
use gateway_messages::{
    RotError, RotWatchdogError as GwRotWatchdogError, SpError,
    SprocketsError as GwSprocketsErr,
    SprotProtocolError as GwSprotProtocolError,
    WatchdogError as GwWatchdogError,
};
use idol_runtime::RequestError;

/// An error returned from a sprot request
#[derive(
    Debug,
    Copy,
    Clone,
    Serialize,
    Deserialize,
    SerializedSize,
    From,
    PartialEq,
    counters::Count,
)]
pub enum SprotError {
    Protocol(#[count(children)] SprotProtocolError),
    Update(#[count(children)] UpdateError),
    Sprockets(#[count(children)] SprocketsError),
    Watchdog(#[count(children)] WatchdogError),
}

impl From<SprotError> for SpError {
    fn from(value: SprotError) -> Self {
        match value {
            SprotError::Protocol(e) => Self::Sprot(e.into()),
            SprotError::Update(e) => Self::Update(e.into()),
            SprotError::Sprockets(e) => Self::Sprockets(e.into()),
            SprotError::Watchdog(e) => Self::Watchdog(e.into()),
        }
    }
}

impl From<SprotError> for RotError {
    fn from(value: SprotError) -> Self {
        match value {
            SprotError::Protocol(e) => Self::Sprot(e.into()),
            SprotError::Update(e) => Self::Update(e.into()),
            SprotError::Sprockets(e) => Self::Sprockets(e.into()),
            SprotError::Watchdog(e) => Self::Watchdog(e.into()),
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
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
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
    // The SP and RoT did not agree on whether the SP is sending
    // a request or waiting for a reply.
    Desynchronized,
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
            SprotProtocolError::Desynchronized => Self::Desynchronized,
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
                matches!(
                    err,
                    InvalidCrc
                        | FlowError
                        | Timeout
                        | TaskRestarted
                        | Deserialization
                        | Desynchronized
                )
            }
            _ => false,
        }
    }
}

// There are currently no other exposed sprockets errors,
// and sprockets isn't in use yet. This is just a place holder.
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
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

#[derive(
    Copy,
    Clone,
    Debug,
    From,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum DumpOrSprotError {
    Sprot(#[count(children)] SprotError),
    Dump(DumperError),
}

impl From<SprotError> for RequestError<DumpOrSprotError> {
    fn from(err: SprotError) -> Self {
        DumpOrSprotError::from(err).into()
    }
}

impl<V> From<DumpOrSprotError> for Result<V, RequestError<DumpOrSprotError>> {
    fn from(err: DumpOrSprotError) -> Self {
        Err(RequestError::Runtime(err))
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    From,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum RawCabooseOrSprotError {
    Sprot(#[count(children)] SprotError),
    Caboose(#[count(children)] RawCabooseError),
}

#[derive(Copy, Clone, Debug, counters::Count)]
pub enum CabooseOrSprotError {
    Sprot(#[count(children)] SprotError),
    Caboose(#[count(children)] CabooseError),
}

impl From<RawCabooseOrSprotError> for CabooseOrSprotError {
    fn from(e: RawCabooseOrSprotError) -> Self {
        match e {
            RawCabooseOrSprotError::Caboose(e) => {
                CabooseOrSprotError::Caboose(e.into())
            }
            RawCabooseOrSprotError::Sprot(e) => CabooseOrSprotError::Sprot(e),
        }
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    From,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum AttestOrSprotError {
    Sprot(#[count(children)] SprotError),
    Attest(#[count(children)] AttestError),
}

impl From<SprotError> for RequestError<AttestOrSprotError> {
    fn from(err: SprotError) -> Self {
        AttestOrSprotError::from(err).into()
    }
}

impl From<SprotProtocolError> for RequestError<AttestOrSprotError> {
    fn from(err: SprotProtocolError) -> Self {
        AttestOrSprotError::Sprot(SprotError::Protocol(err)).into()
    }
}

impl<V> From<AttestOrSprotError>
    for Result<V, RequestError<AttestOrSprotError>>
{
    fn from(err: AttestOrSprotError) -> Self {
        Err(RequestError::Runtime(err))
    }
}

impl From<idol_runtime::ServerDeath> for AttestOrSprotError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        AttestOrSprotError::Attest(AttestError::TaskRestarted)
    }
}

impl From<AttestOrSprotError> for AttestDataSprotError {
    fn from(err: AttestOrSprotError) -> Self {
        match err {
            AttestOrSprotError::Sprot(e) => match e {
                SprotError::Protocol(e1) => match e1 {
                    SprotProtocolError::InvalidCrc => Self::ProtocolInvalidCrc,
                    SprotProtocolError::FlowError => Self::ProtocolFlowError,
                    SprotProtocolError::UnsupportedProtocol => {
                        Self::ProtocolUnsupportedProtocol
                    }
                    SprotProtocolError::BadMessageType => {
                        Self::ProtocolBadMessageType
                    }
                    SprotProtocolError::BadMessageLength => {
                        Self::ProtocolBadMessageLength
                    }
                    SprotProtocolError::CannotAssertCSn => {
                        Self::ProtocolCannotAssertCSn
                    }
                    SprotProtocolError::Timeout => Self::ProtocolTimeout,
                    SprotProtocolError::Deserialization => {
                        Self::ProtocolDeserialization
                    }
                    SprotProtocolError::RotIrqRemainsAsserted => {
                        Self::ProtocolRotIrqRemainsAsserted
                    }
                    SprotProtocolError::UnexpectedResponse => {
                        Self::ProtocolUnexpectedResponse
                    }
                    SprotProtocolError::BadUpdateStatus => {
                        Self::ProtocolBadUpdateStatus
                    }
                    SprotProtocolError::TaskRestarted => {
                        Self::ProtocolTaskRestarted
                    }
                    SprotProtocolError::Desynchronized => {
                        Self::ProtocolDesynchronized
                    }
                },
                // We should never return these but it's safer to return an
                // enum just in case these come up
                SprotError::Update(_) => Self::UpdateError,
                SprotError::Sprockets(_) => Self::SprocketsError,
                SprotError::Watchdog(_) => Self::WatchdogError,
            },
            AttestOrSprotError::Attest(e) => match e {
                AttestError::CertTooBig => Self::AttestCertTooBig,
                AttestError::InvalidCertIndex => Self::AttestInvalidCertIndex,
                AttestError::NoCerts => Self::AttestNoCerts,
                AttestError::OutOfRange => Self::AttestOutOfRange,
                AttestError::LogFull => Self::AttestLogFull,
                AttestError::LogTooBig => Self::AttestLogTooBig,
                AttestError::TaskRestarted => Self::AttestTaskRestarted,
                AttestError::BadLease => Self::AttestBadLease,
                AttestError::UnsupportedAlgorithm => {
                    Self::AttestUnsupportedAlgorithm
                }
                AttestError::SerializeLog => Self::AttestSerializeLog,
                AttestError::SerializeSignature => {
                    Self::AttestSerializeSignature
                }
                AttestError::SignatureTooBig => Self::AttestSignatureTooBig,
                AttestError::ReservedLogSlot => Self::AttestLogSlotReserved,
            },
        }
    }
}

// Added in sprot protocol version 5
#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    counters::Count,
)]
pub enum WatchdogError {
    /// Could not control the SP over SWD
    DongleDetected,
    /// Raw `SpCtrlError` value
    Other(u32),
}

impl From<WatchdogError> for GwWatchdogError {
    fn from(s: WatchdogError) -> Self {
        match s {
            WatchdogError::DongleDetected => {
                Self::Rot(GwRotWatchdogError::DongleDetected)
            }
            WatchdogError::Other(i) => Self::Rot(GwRotWatchdogError::Other(i)),
        }
    }
}

// Added in protocol v6
#[derive(
    Copy, Clone, Debug, Serialize, Deserialize, SerializedSize, counters::Count,
)]
pub enum StateError {
    ReadCmpa(UpdateError),
    ReadCfpa(UpdateError),
    BadRevoke { revoke: u8 },
}

#[derive(
    Copy,
    Clone,
    Debug,
    From,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum StateOrSprotError {
    Sprot(#[count(children)] SprotError),
    State(#[count(children)] StateError),
}
