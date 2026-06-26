// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types for the I2C server client API
//!
//! This crate works on both the host and embedded system, so it can be used in
//! host-side tests.

#![no_std]

use hubpack::SerializedSize;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;
use serde::{Deserialize, Serialize};

use derive_idol_err::IdolError;
use enum_kinds::EnumKind;

#[derive(FromPrimitive, Eq, PartialEq)]
pub enum Op {
    WriteRead = 1,

    /// In a `WriteReadBlock` operation, only the **final read** is an SMBus
    /// block operation.
    ///
    /// All writes and all other read operations are normal (non-block)
    /// operations.
    ///
    /// We don't need a special way to perform block writes, because they can be
    /// constructed by the caller without cooperation from the driver.
    /// Specifically, the caller can construct the array `[reg, size, data[0],
    /// data[1], ...]` and pass it to a normal `WriteRead` operation.
    ///
    /// If we encounter a device which requires multiple block reads in a row
    /// without interruption, this logic would not work, but that would be a
    /// very strange device indeed.
    WriteReadBlock = 2,
}

/// The response code returned from the I2C server.  These response codes pretty
/// specific, not because the caller is expected to necessarily handle them
/// differently, but to give upstack software some modicum of context
/// surrounding the error.
#[derive(
    Copy,
    Clone,
    Debug,
    EnumKind,
    FromPrimitive,
    Eq,
    PartialEq,
    IdolError,
    Serialize,
    Deserialize,
    SerializedSize,
    counters::Count,
)]
#[enum_kind(ResponseCodeU8, derive(counters::Count))]
#[repr(u32)]
pub enum ResponseCode {
    /// Bad response from server
    BadResponse = 1,
    /// Bad argument sent to server
    BadArg,
    /// Indicated I2C device is invalid
    NoDevice,
    /// Indicated I2C controller is invalid
    BadController,
    /// Device address is reserved
    ReservedAddress,
    /// Indicated port is invalid
    BadPort,
    /// Device does not have indicated register
    NoRegister,
    /// Indicated mux is an invalid mux identifier
    BadMux,
    /// Indicated segment is an invalid segment identifier
    BadSegment,
    /// Indicated mux does not exist on this controller
    MuxNotFound,
    /// Indicated segment does not exist on this controller
    SegmentNotFound,
    /// Segment disconnected during operation
    SegmentDisconnected,
    /// Mux disconnected during operation
    MuxDisconnected,
    /// No device at address used for mux in-band management
    MuxMissing,
    /// Register used for mux in-band management is invalid
    BadMuxRegister,
    /// I2C bus was spontaneously reset during operation
    BusReset,
    /// I2C bus was reset during a mux in-band management operation
    BusResetMux,
    /// I2C bus locked up and was reset
    BusLocked,
    /// I2C bus locked up during in-band management operation and was reset
    BusLockedMux,
    /// I2C controller appeared to be busy and was reset
    ControllerBusy,
    /// I2C bus error
    BusError,
    /// Bad device state of unknown origin
    BadDeviceState,
    /// Requested operation is not supported
    OperationNotSupported,
    /// Illegal number of leases
    IllegalLeaseCount,
    /// Too much data -- or not enough buffer
    TooMuchData,
}

impl ResponseCode {
    /// A hint whether this response code is indicative of an error that may
    /// succeed on retry
    ///
    /// The semantics of this are best-effort, and are generally geared towards
    /// "immediate" retries. For example, a device missing may work again in
    /// the future if it is re-attached, but is unlikely to work immediately.
    pub fn retry_hint(&self) -> bool {
        // TODO: This needs opinions
        match self {
            // No, a retry will probably not help here
            ResponseCode::BadResponse
            | ResponseCode::BadArg
            | ResponseCode::NoDevice
            | ResponseCode::BadController
            | ResponseCode::ReservedAddress
            | ResponseCode::BadPort
            | ResponseCode::NoRegister
            | ResponseCode::BadMux
            | ResponseCode::BadSegment
            | ResponseCode::MuxNotFound
            | ResponseCode::SegmentNotFound
            | ResponseCode::SegmentDisconnected
            | ResponseCode::MuxDisconnected
            | ResponseCode::MuxMissing
            | ResponseCode::BadMuxRegister
            | ResponseCode::BadDeviceState
            | ResponseCode::OperationNotSupported
            | ResponseCode::IllegalLeaseCount
            | ResponseCode::TooMuchData => false,

            // Yes, a retry may help here
            ResponseCode::BusReset
            | ResponseCode::BusError
            | ResponseCode::BusResetMux
            | ResponseCode::BusLocked
            | ResponseCode::BusLockedMux
            | ResponseCode::ControllerBusy => true,
        }
    }
}

///
/// The controller for a given I2C device. The numbering here should be
/// assumed to follow the numbering for the peripheral as described by the
/// microcontroller.
///
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    SerializedSize,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Controller {
    I2C0 = 0,
    I2C1 = 1,
    I2C2 = 2,
    I2C3 = 3,
    I2C4 = 4,
    I2C5 = 5,
    I2C6 = 6,
    I2C7 = 7,
    Mock = 0xff,
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
#[allow(clippy::unusual_byte_groupings)]
pub enum ReservedAddress {
    GeneralCall = 0b0000_000,
    CBUSAddress = 0b0000_001,
    FutureBus = 0b0000_010,
    FuturePurposes = 0b0000_011,
    HighSpeedReserved00 = 0b0000_100,
    HighSpeedReserved01 = 0b0000_101,
    HighSpeedReserved10 = 0b0000_110,
    HighSpeedReserved11 = 0b0000_111,
    TenBit00 = 0b1111_100,
    TenBit01 = 0b1111_101,
    TenBit10 = 0b1111_110,
    TenBit11 = 0b1111_111,
}

///
/// The port index for a given I2C device.  Some controllers can have multiple
/// ports (which themselves are connected to different I2C buses), but only
/// one port can be active at a time.  For these controllers, a port index
/// must be specified.  The mapping between these indices and values that make
/// sense in terms of the I2C controller (e.g., the lettered port) is
/// specified in the application configuration; to minimize confusion, the
/// letter should generally match the GPIO port of the I2C bus (assuming that
/// GPIO ports are lettered), but these values are in fact strings and can
/// take any value.  Note that if a given I2C controller straddles two ports,
/// the port of SDA should generally be used when naming the port; if a GPIO
/// port contains multiple SDAs on it from the same controller, the
/// letter/number convention should be used (e.g., "B1") -- but this is purely
/// convention.
///
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
pub struct PortIndex(pub u8);

///
/// A multiplexer identifier for a given I2C bus.  Multiplexer identifiers
/// need not start at 0.
///
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    SerializedSize,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Mux {
    M1 = 1,
    M2 = 2,
    M3 = 3,
    M4 = 4,
    M5 = 5,
}

///
/// A segment identifier on a given multiplexer.  Segment identifiers
/// need not start at 0.
///
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    SerializedSize,
    Serialize,
    Deserialize,
)]
#[repr(u8)]
pub enum Segment {
    S1 = 1,
    S2 = 2,
    S3 = 3,
    S4 = 4,
    S5 = 5,
    S6 = 6,
    S7 = 7,
    S8 = 8,
    S9 = 9,
    S10 = 10,
    S11 = 11,
    S12 = 12,
    S13 = 13,
    S14 = 14,
    S15 = 15,
    S16 = 16,
}

pub mod pmbus_status {
    /// Type that denotes the STATUS registers supported for a given PMBus
    /// device
    ///
    /// This is typically code-generated at build time using information
    /// from the `pmbus` crate.
    #[derive(Debug, PartialEq, Clone, Copy)]
    pub struct Capabilities(pub u32);

    impl Capabilities {
        pub const STATUS_WORD: Self = Self(1 << 0);
        pub const STATUS_VOUT: Self = Self(1 << 1);
        pub const STATUS_IOUT: Self = Self(1 << 2);
        pub const STATUS_TEMPERATURE: Self = Self(1 << 3);
        pub const STATUS_CML: Self = Self(1 << 4);
        pub const STATUS_OTHER: Self = Self(1 << 5);
        pub const STATUS_INPUT: Self = Self(1 << 6);
        pub const STATUS_MFR_SPECIFIC: Self = Self(1 << 7);
        pub const STATUS_FANS_1_2: Self = Self(1 << 8);
        pub const STATUS_FANS_3_4: Self = Self(1 << 9);

        /// Does this capability support all capabilities of `other`?
        ///
        /// `self` may support *more* capabilities than `other`, but
        /// not the other way around.
        #[inline]
        pub const fn supports(&self, other: &Self) -> bool {
            (self.0 & other.0) == other.0
        }
    }
}
