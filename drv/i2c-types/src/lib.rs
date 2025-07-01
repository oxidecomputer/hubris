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
    /// The device address was NACKed, implying that it is missing, unreachable,
    /// or not responding.
    NoDevice,
    /// An I2C controller number sent to the I2C server is not valid.
    BadController,
    /// The device address sent to the I2C server is reserved per the I2C
    /// standard.
    ReservedAddress,
    /// The port number sent to the I2C server is invalid.
    BadPort,
    /// A byte written to the device was NACKed, which may indicate an invalid
    /// parameter, end of received data, etc.
    NoRegister,
    /// A mux value sent to the I2C server is not valid.
    BadMux,
    /// Segment identifier sent to the I2C server is not valid.
    BadSegment,
    /// A mux value sent to the I2C server is possibly valid for some other
    /// machine, but not this one. In practice this should probably be handled
    /// as equivalent to `BadMux`.
    MuxNotFound,
    /// A segment value sent to the I2C server is possibly value for some other
    /// machine, but not this one. In practice this should probably be handled
    /// as equivalent to `BadSegment`.
    SegmentNotFound,
    /// A mux refused to connect the segment you've requested because it's being
    /// held low by a downstream device.
    SegmentDisconnected,
    /// A mux failed to connect the segment you've requested, but not because it
    /// was being held low. In practice, this can basically only happen if the
    /// I2C driver is preempted long enough for a mux's activity timeout to
    /// fire, _and_ a device failed and started asynchronously holding the line
    /// low during this time.
    MuxDisconnected,
    /// The mux's address was NACKed, implying that it is missing, unreachable,
    /// or not responding.
    MuxMissing,
    /// A byte written to a mux was NACKed.
    BadMuxRegister,
    /// We detected "arbitration lost," meaning that SDA was sampled as low at a
    /// clock edge when we were attempting to leave it high.
    BusReset,
    /// We detected "arbitration lost" during an operation on a mux.
    BusResetMux,
    /// The SMBus timeout hardware fired, implying that a device held the bus
    /// low for too long, or (perhaps more likely) that our driver got preempted
    /// while processing events, likely by a crash dump.
    BusLocked,
    /// A `BusLocked` (timeout) event occurred while attempting to program a mux
    /// on the way to the device you requested.
    BusLockedMux,
    /// The I2C controller was unexpectedly indicating "busy" and this condition
    /// did not clear within the expected (brief) period of time.
    ControllerBusy,
    /// An I2C protocol timing violation has been detected on the bus. In
    /// practice, this means that (1) we were actively driving the bus, and (2) a start or stop condition was observed unexpectedly, and (3) it did not happen at a 9-bit frame boundary. This usually indicates a glitch, either from us or a device.
    BusError,
    /// Unspecified device-specific state error.
    BadDeviceState,
    /// Specific to the `power` task, which reuses this error type for some
    /// reason: the PMBus operation you have requested is not implemented by the
    /// target device.
    OperationNotSupported,
    /// You have sent the I2C server fewer than 2 leases, or an odd number of
    /// leases.
    IllegalLeaseCount,
    /// A block transfer (SMbus or PMbus) tried to move too much data into a
    /// device, or tried to read a block into a lease that turned out to be too
    /// small.
    TooMuchData,
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
}
