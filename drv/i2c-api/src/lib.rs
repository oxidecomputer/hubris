// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the I2C server
//!
//! This API allows for access to I2C devices.  The actual I2C bus
//! communication occurs in a disjoint I2C server task; this API handles
//! marshalling (and unmarshalling) of messages to (and replies from) this
//! task to perform I2C operations.
//!
//! # I2C devices
//!
//! An I2C device is uniquely identified by a 5-tuple:
//!
//! - The I2C controller in the MCU
//! - The port for that controller, identifying a bus
//! - The multiplexer on the specified I2C bus, if any
//! - The segment on the multiplexer, if a multiplexer is specified
//! - The address of the device itself
//!

#![no_std]

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use zerocopy::{AsBytes, FromBytes};

use derive_idol_err::IdolError;
use userlib::*;

#[derive(FromPrimitive, Eq, PartialEq)]
pub enum Op {
    WriteRead = 1,
    WriteReadBlock = 2,
    SelectedMuxSegment = 3,
}

/// The response code returned from the I2C server.  These response codes pretty
/// specific, not because the caller is expected to necessarily handle them
/// differently, but to give upstack software some modicum of context
/// surrounding the error.
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(u32)]
pub enum ResponseCode {
    /// Bad response from server
    BadResponse = 1,
    /// Bad argument sent to server
    BadArg = 2,
    /// Indicated I2C device is invalid
    NoDevice = 3,
    /// Indicated I2C controller is invalid
    BadController = 4,
    /// Device address is reserved
    ReservedAddress = 5,
    /// Indicated port is invalid
    BadPort = 6,
    /// Device does not have indicated register
    NoRegister = 8,
    /// Indicated mux is an invalid mux identifier
    BadMux = 9,
    /// Indicated segment is an invalid segment identifier
    BadSegment = 10,
    /// Indicated mux does not exist on this controller
    MuxNotFound = 11,
    /// Indicated segment does not exist on this controller
    SegmentNotFound = 12,
    /// Segment disconnected during operation
    SegmentDisconnected = 13,
    /// Mux disconnected during operation
    MuxDisconnected = 14,
    /// Address used for mux in-band management is invalid
    BadMuxAddress = 15,
    /// Register used for mux in-band management is invalid
    BadMuxRegister = 16,
    /// I2C bus was spontaneously reset during operation
    BusReset = 17,
    /// I2C bus was reset during a mux in-band management operation
    BusResetMux = 18,
    /// I2C bus locked up and was reset
    BusLocked = 19,
    /// I2C bus locked up during in-band management operation and was reset
    BusLockedMux = 20,
    /// I2C controller appeared to be busy and was reset
    ControllerBusy = 21,
    /// I2C bus error
    BusError = 22,
    /// Bad device state of unknown origin
    BadDeviceState = 23,
    /// Bad return value for selected mux/segment
    BadSelectedMux = 24,
    /// Requested operation is not supported
    OperationNotSupported = 25,
    /// Illegal number of leases
    IllegalLeaseCount = 26,
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

///
/// The 5-tuple that uniquely identifies an I2C device.  The multiplexer and
/// the segment are optional, but if one is present, the other must be.
///
#[derive(Copy, Clone, Debug)]
pub struct I2cDevice {
    pub task: TaskId,
    pub controller: Controller,
    pub port: PortIndex,
    pub segment: Option<(Mux, Segment)>,
    pub address: u8,
}

type I2cMessage = (u8, Controller, PortIndex, Option<(Mux, Segment)>);

pub trait Marshal<T> {
    fn marshal(&self) -> T;
    fn unmarshal(val: &T) -> Result<Self, ResponseCode>
    where
        Self: Sized;
}

impl Marshal<[u8; 4]> for I2cMessage {
    fn marshal(&self) -> [u8; 4] {
        [
            self.0,
            self.1 as u8,
            self.2 .0,
            match self.3 {
                Some((mux, seg)) => {
                    0b1000_0000 | ((mux as u8) << 4) | (seg as u8)
                }
                None => 0,
            },
        ]
    }
    fn unmarshal(val: &[u8; 4]) -> Result<Self, ResponseCode> {
        Ok((
            val[0],
            Controller::from_u8(val[1]).ok_or(ResponseCode::BadController)?,
            PortIndex(val[2]),
            if val[3] == 0 {
                None
            } else {
                Some((
                    Mux::from_u8((val[3] & 0b0111_0000) >> 4)
                        .ok_or(ResponseCode::BadMux)?,
                    Segment::from_u8(val[3] & 0b0000_1111)
                        .ok_or(ResponseCode::BadSegment)?,
                ))
            },
        ))
    }
}

impl core::fmt::Display for I2cDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let addr = self.address;

        match self.segment {
            None => {
                write!(f, "{:?}:{:?} {:#x}", self.controller, self.port, addr)
            }
            Some((mux, segment)) => {
                write!(
                    f,
                    "{:?}:{:?}, {:?}:{:?} {:#x}",
                    self.controller, self.port, mux, segment, addr
                )
            }
        }
    }
}

impl I2cDevice {
    ///
    /// Return a new [`I2cDevice`], given a 5-tuple identifying a device plus
    /// a task identifier for the I2C driver.  This will not make any IPC
    /// requests to the specified task.
    ///
    pub fn new(
        task: TaskId,
        controller: Controller,
        port: PortIndex,
        segment: Option<(Mux, Segment)>,
        address: u8,
    ) -> Self {
        Self {
            task,
            controller,
            port,
            segment,
            address,
        }
    }
}

impl I2cDevice {
    ///
    /// Reads a register, with register address of type R and value of type V.
    ///
    /// ## Register definition
    ///
    /// Most devices have a notion of a different kinds of values that can be
    /// read; the numerical value of the desired kind is written to the
    /// device, and then the device replies by writing back the desired value.
    /// This notion is often called a "register", but "pointer" and "address"
    /// are also common.  Register values are often 8-bit, but can also be
    /// larger; the type of the register value is parameterized to afford this
    /// flexibility.
    ///
    /// ## Error handling
    ///
    /// On failure, a [`ResponseCode`] will indicate more detail.
    ///
    pub fn read_reg<R: AsBytes, V: AsBytes + FromBytes>(
        &self,
        reg: R,
    ) -> Result<V, ResponseCode> {
        let mut val = V::new_zeroed();
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::from(reg.as_bytes()), Lease::from(val.as_bytes_mut())],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

    ///
    /// Like [`read_reg`], but instead of returning a value, reads as many
    /// bytes as the device will send into a specified slice, returning the
    /// number of bytes read.
    ///
    pub fn read_reg_into<R: AsBytes>(
        &self,
        reg: R,
        buf: &mut [u8],
    ) -> Result<usize, ResponseCode> {
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::from(reg.as_bytes()), Lease::from(buf)],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(response)
        }
    }

    ///
    /// Performs an SMBus block read (in which the first byte returned from
    /// the device contains the total number of bytes to read) into the
    /// specified buffer, returning the total number of bytes read.  Note
    /// that the byte count is only returned from the function; it is *not*
    /// present as the payload's first byte.
    ///
    pub fn read_block<R: AsBytes>(
        &self,
        reg: R,
        buf: &mut [u8],
    ) -> Result<usize, ResponseCode> {
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteReadBlock as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::from(reg.as_bytes()), Lease::from(buf)],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(response)
        }
    }

    ///
    /// Reads from a device *without* first doing a write.  This is probably
    /// not what you want, and only exists because there exist some nutty
    /// devices whose registers are not addressable (*glares at MAX7358*).
    /// (And indeed, on these devices, attempting to read a register will
    /// in fact overwrite the contents of the first two registers.)
    ///
    pub fn read<V: AsBytes + FromBytes>(&self) -> Result<V, ResponseCode> {
        let mut val = V::new_zeroed();
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::read_only(&[]), Lease::from(val.as_bytes_mut())],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

    ///
    /// Reads from a device *without* first doing a write.  This is like
    /// [`read`], but will read as many bytes as the device will offer into
    /// the specified mutable slice, returning the number of bytes read.
    ///
    pub fn read_into(&self, buf: &mut [u8]) -> Result<usize, ResponseCode> {
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::read_only(&[]), Lease::from(buf)],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(response)
        }
    }

    ///
    /// Writes a buffer to a device. Unlike a register read, this will not
    /// perform any follow-up reads.
    ///
    pub fn write(&self, buffer: &[u8]) -> Result<(), ResponseCode> {
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[Lease::from(buffer), Lease::read_only(&[])],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(())
        }
    }

    ///
    /// Writes a buffer, and then performs a subsequent register read.  These
    /// are not performed as a single I2C transaction (that is, it is not a
    /// repeated start) -- but the effect is the same in that the server does
    /// these operations without an intervening receive (assuring that the
    /// write can modify device state that the subsequent register read can
    /// assume).
    ///
    pub fn write_read_reg<R: AsBytes, V: AsBytes + FromBytes>(
        &self,
        reg: R,
        buffer: &[u8],
    ) -> Result<V, ResponseCode> {
        let mut val = V::new_zeroed();
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[
                Lease::from(buffer),
                Lease::read_only(&[]),
                Lease::from(reg.as_bytes()),
                Lease::from(val.as_bytes_mut()),
            ],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

    ///
    /// Writes one buffer to a device, and then another.  These are not
    /// performed as a single I2C transaction (that is, it is not a repeated
    /// start) -- but the effect is the same in that the server does these
    /// operations without an intervening receive (assuring that the write can
    /// modify device state that the subsequent write can assume).
    ///
    pub fn write_write(
        &self,
        first: &[u8],
        second: &[u8],
    ) -> Result<(), ResponseCode> {
        let mut response = 0_usize;

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            response.as_bytes_mut(),
            &[
                Lease::from(first),
                Lease::read_only(&[]),
                Lease::from(second),
                Lease::read_only(&[]),
            ],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(())
        }
    }

    pub fn selected_mux_segment(
        &self,
    ) -> Result<Option<(Mux, Segment)>, ResponseCode> {
        let mut response = [0u8; 4];

        let (code, _) = sys_send(
            self.task,
            Op::SelectedMuxSegment as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                None,
            )),
            response.as_bytes_mut(),
            &[],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            let (address, controller, port, mux) =
                Marshal::unmarshal(&response)?;

            if controller != self.controller
                || address != self.address
                || port != self.port
            {
                Err(ResponseCode::BadSelectedMux)
            } else {
                Ok(mux)
            }
        }
    }
}
