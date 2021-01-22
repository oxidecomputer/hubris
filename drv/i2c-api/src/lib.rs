//! Client API for the I2C server

#![no_std]

use zerocopy::{AsBytes, FromBytes};

use userlib::*;

#[derive(FromPrimitive)]
pub enum Op {
    WriteRead = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum ResponseCode {
    Dead = core::u32::MAX,
    BadResponse = 1,
    BadArg = 2,
    NoDevice = 3,
    BadController = 4,
    ReservedAddress = 5,
    BadPort = 6,
    BadDefaultPort = 7,
    NoRegister = 8,
}

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

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
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

#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum Mux {
    M0 = 0,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum Segment {
    S0 = 0,
}

#[derive(Clone, Debug)]
pub struct I2c {
    pub task: TaskId,
    pub controller: Controller,
    pub port: Port,
    pub segment: Option<(Mux, Segment)>,
    pub address: u8,
}

impl I2c {
    pub fn new(
        task: TaskId,
        controller: Controller,
        port: Port,
        segment: Option<(Mux, Segment)>,
        address: u8
    ) -> Self {
        Self {
            task: task,
            controller: controller,
            port: port,
            segment: segment,
            address: address,
        }
    }
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

impl I2c {
    /// Reads a register, with register address of type R and value of type V
    pub fn read_reg<R: AsBytes, V: Default + AsBytes + FromBytes>(
        &self,
        reg: R,
    ) -> Result<V, ResponseCode> {
        let mut val = V::default();

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &[self.address, self.controller as u8, self.port as u8],
            &mut [],
            &[Lease::from(reg.as_bytes()), Lease::from(val.as_bytes_mut())],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code).ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(val)
        }
    }

    /// Writes a buffer
    pub fn write(&self, buffer: &[u8]) -> Result<(), ResponseCode> {
        let empty = [0u8; 1];

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &[self.address, self.controller as u8, self.port as u8],
            &mut [],
            &[Lease::from(buffer), Lease::from(&empty[0..0])],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code).ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(())
        }
    }
}
