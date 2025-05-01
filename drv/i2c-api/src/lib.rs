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

use zerocopy::{FromBytes, Immutable, IntoBytes};

pub use drv_i2c_types::*;
use userlib::{sys_send, FromPrimitive, Lease, TaskId};

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
    fn response_code<V>(&self, code: u32, val: V) -> Result<V, ResponseCode> {
        if code != 0 {
            if let Some(_g) = userlib::extract_new_generation(code) {
                panic!("i2c reset");
            }

            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

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
    pub fn read_reg<R, V>(&self, reg: R) -> Result<V, ResponseCode>
    where
        R: IntoBytes + Immutable,
        V: IntoBytes + FromBytes,
    {
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
            response.as_mut_bytes(),
            &[Lease::from(reg.as_bytes()), Lease::from(val.as_mut_bytes())],
        );

        self.response_code(code, val)
    }

    ///
    /// Like [`read_reg`], but instead of returning a value, reads as many
    /// bytes as the device will send into a specified slice, returning the
    /// number of bytes read.
    ///
    pub fn read_reg_into<R: IntoBytes + Immutable>(
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
            response.as_mut_bytes(),
            &[Lease::from(reg.as_bytes()), Lease::from(buf)],
        );

        self.response_code(code, response)
    }

    ///
    /// Performs an SMBus block read (in which the first byte returned from
    /// the device contains the total number of bytes to read) into the
    /// specified buffer, returning the total number of bytes read.  Note
    /// that the byte count is only returned from the function; it is *not*
    /// present as the payload's first byte.
    ///
    pub fn read_block<R: IntoBytes + Immutable>(
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
            response.as_mut_bytes(),
            &[Lease::from(reg.as_bytes()), Lease::from(buf)],
        );

        self.response_code(code, response)
    }

    ///
    /// Reads from a device *without* first doing a write.  This is probably
    /// not what you want, and only exists because there exist some nutty
    /// devices whose registers are not addressable (*glares at MAX7358*).
    /// (And indeed, on these devices, attempting to read a register will
    /// in fact overwrite the contents of the first two registers.)
    ///
    pub fn read<V: IntoBytes + FromBytes>(&self) -> Result<V, ResponseCode> {
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
            response.as_mut_bytes(),
            &[Lease::read_only(&[]), Lease::from(val.as_mut_bytes())],
        );

        self.response_code(code, val)
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
            response.as_mut_bytes(),
            &[Lease::read_only(&[]), Lease::from(buf)],
        );

        self.response_code(code, response)
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
            response.as_mut_bytes(),
            &[Lease::from(buffer), Lease::read_only(&[])],
        );

        self.response_code(code, ())
    }

    ///
    /// Writes a buffer, and then performs a subsequent register read.  These
    /// are not performed as a single I2C transaction (that is, it is not a
    /// repeated start) -- but the effect is the same in that the server does
    /// these operations without an intervening receive (assuring that the
    /// write can modify device state that the subsequent register read can
    /// assume).
    ///
    pub fn write_read_reg<R, V>(
        &self,
        reg: R,
        buffer: &[u8],
    ) -> Result<V, ResponseCode>
    where
        R: IntoBytes + Immutable,
        V: IntoBytes + FromBytes,
    {
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
            response.as_mut_bytes(),
            &[
                Lease::from(buffer),
                Lease::read_only(&[]),
                Lease::from(reg.as_bytes()),
                Lease::from(val.as_mut_bytes()),
            ],
        );

        self.response_code(code, val)
    }

    ///
    /// Performs a write followed by an SMBus block read (in which the first
    /// byte returned from the device contains the total number of bytes to
    /// read) into the specified buffer, returning the total number of bytes
    /// read.  Note that the byte count is only returned from the function; it
    /// is *not* present as the payload's first byte.
    ///
    /// The write and read are not performed as a single I2C transaction (that
    /// is, it is not a repeated start) -- but the effect is the same in that
    /// the server does these operations without an intervening receive
    /// (assuring that the write can modify device state that the subsequent
    /// read can assume).
    ///
    pub fn write_read_block<R: IntoBytes + Immutable>(
        &self,
        reg: R,
        buffer: &[u8],
        out: &mut [u8],
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
            response.as_mut_bytes(),
            &[
                Lease::from(buffer),
                Lease::read_only(&[]),
                Lease::from(reg.as_bytes()),
                Lease::from(out),
            ],
        );

        self.response_code(code, response)
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
            response.as_mut_bytes(),
            &[
                Lease::from(first),
                Lease::read_only(&[]),
                Lease::from(second),
                Lease::read_only(&[]),
            ],
        );

        self.response_code(code, ())
    }

    ///
    /// Writes one buffer to a device, and then another, and then performs a
    /// register read.  As with [`write_read_reg`] and [`write_write`], these
    /// are not performed as a single I2C transaction, but the effect is the
    /// same in that the server does these operations without an intervening
    /// receive.  This is to accommodate devices that have multiple axes of
    /// configuration (e.g., regulators that have both rail and phase).
    ///
    pub fn write_write_read_reg<R, V>(
        &self,
        reg: R,
        first: &[u8],
        second: &[u8],
    ) -> Result<V, ResponseCode>
    where
        R: IntoBytes + Immutable,
        V: IntoBytes + FromBytes,
    {
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
            response.as_mut_bytes(),
            &[
                Lease::from(first),
                Lease::read_only(&[]),
                Lease::from(second),
                Lease::read_only(&[]),
                Lease::from(reg.as_bytes()),
                Lease::from(val.as_mut_bytes()),
            ],
        );

        self.response_code(code, val)
    }
}
