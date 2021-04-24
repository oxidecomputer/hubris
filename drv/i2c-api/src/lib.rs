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

use zerocopy::{AsBytes, FromBytes};

use userlib::*;

#[derive(FromPrimitive)]
pub enum Op {
    WriteRead = 1,
}

/// The response code returned from the I2C controller (or from the kernel in
/// the case of [`ResponseCode::Dead`]).  These response codes pretty specific,
/// not because the caller is expected to necessarily handle them differently,
/// but to give upstack software some modicum of context surrounding the error.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum ResponseCode {
    /// Server has died
    Dead = core::u32::MAX,
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
    /// Inidcated port is invalid
    BadPort = 6,
    /// Default port indicated, but port must be specified
    BadDefaultPort = 7,
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
    /// I2C controller appeared to be locked and was reset
    ControllerLocked = 21,
}

///
/// The controller for a given I2C device. The numbering here should be
/// assumed to follow the numbering for the peripheral as described by the
/// microcontroller.
///
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
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

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
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
/// The port for a given I2C device.  Some controllers can have multiple
/// ports (which themselves are connected to different I2C busses), but only
/// one port can be active at a time.  For these controllers, a port must
/// be specified (generally lettered).  For controllers that have only one
/// port, [`Port::Default`] should be specified.
///
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum Port {
    Default = 0,
    A = 1,
    B = 2,
    C = 3,
    D = 4,
    E = 5,
    F = 6,
    G = 7,
    H = 8,
    I = 9,
    J = 10,
    K = 11,
}

///
/// A multiplexer for a given I2C device.  Multiplexers are numbered starting
/// from 1.
///
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum Mux {
    M1 = 1,
}

///
/// A segment on a given multiplexer.  Segments are nubered starting from 1.
///
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
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
    pub port: Port,
    pub segment: Option<(Mux, Segment)>,
    pub address: u8,
}

type I2cMessage = (u8, Controller, Port, Option<(Mux, Segment)>);

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
            self.2 as u8,
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
            Port::from_u8(val[2]).ok_or(ResponseCode::BadPort)?,
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

        match (self.port, self.segment) {
            (Port::Default, None) => {
                write!(f, "{:?} 0x{:x}", self.controller, addr)
            }
            (Port::Default, Some((mux, segment))) => {
                write!(
                    f,
                    "{:?}, {:?}:{:?} 0x{:x}",
                    self.controller, mux, segment, addr
                )
            }
            (_, None) => {
                write!(f, "{:?}:{:?} 0x{:x}", self.controller, self.port, addr)
            }
            (_, Some((mux, segment))) => {
                write!(
                    f,
                    "{:?}:{:?}, {:?}:{:?} 0x{:x}",
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
        port: Port,
        segment: Option<(Mux, Segment)>,
        address: u8,
    ) -> Self {
        Self {
            task: task,
            controller: controller,
            port: port,
            segment: segment,
            address: address,
        }
    }

    ///
    /// Returns a mocked I2C device that does not correspond to an actual
    /// device.  This is for purposes of allowing standalone builds of tasks;
    /// production code should not have such a device, and all operations
    /// would be expected to fail with a `ResponseCode::BadController`.
    ///
    #[cfg(feature = "standalone")]
    pub fn mock(task: TaskId) -> Self {
        Self {
            task: task,
            controller: Controller::Mock,
            port: Port::Default,
            segment: None,
            address: 0,
        }
    }
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
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
    pub fn read_reg<R: AsBytes, V: Default + AsBytes + FromBytes>(
        &self,
        reg: R,
    ) -> Result<V, ResponseCode> {
        let mut val = V::default();

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            &mut [],
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
    /// Reads from a device *without* first doing a write.  This is probably
    /// not what you want, and only exists because there exist some nutty
    /// devices whose registers are not addressable (*glares at MAX7358*).
    /// (And indeed, on these devices, attempting to read a register will
    /// in fact overwrite the contents of the first two registers.)
    ///
    pub fn read<V: Default + AsBytes + FromBytes>(
        &self,
    ) -> Result<V, ResponseCode> {
        let empty = [0u8; 1];
        let mut val = V::default();

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            &mut [],
            &[Lease::from(&empty[0..0]), Lease::from(val.as_bytes_mut())],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

    ///
    /// Writes a buffer to a device. Unlike a register read, this will not
    /// perform any follow-up reads.
    ///
    pub fn write(&self, buffer: &[u8]) -> Result<(), ResponseCode> {
        let empty = [0u8; 1];

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &Marshal::marshal(&(
                self.address,
                self.controller,
                self.port,
                self.segment,
            )),
            &mut [],
            &[Lease::from(buffer), Lease::from(&empty[0..0])],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(())
        }
    }
}
