//! Client API for the I2C server

#![no_std]

use zerocopy::{AsBytes, FromBytes};

use userlib::*;

#[derive(FromPrimitive)]
pub enum Op {
    WriteRead = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
pub enum Interface {
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

#[derive(Clone, Debug)]
pub struct I2c {
    pub task: TaskId,
    pub interface: Interface,
    pub address: u8,
}

impl I2c {
    pub fn new(task: TaskId, interface: Interface, address: u8) -> Self {
        Self {
            task: task,
            interface: interface,
            address: address,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum I2cError {
    Dead = !0,
    BadArg,
    NoDevice,
    Busy,
    BadInterface,
    ReservedAddress,
}

impl From<u32> for I2cError {
    fn from(x: u32) -> Self {
        match x {
            core::u32::MAX => I2cError::Dead,
            1 => I2cError::BadArg,
            2 => I2cError::NoDevice,
            3 => I2cError::Busy,
            4 => I2cError::BadInterface,
            5 => I2cError::ReservedAddress,
            _ => panic!(),
        }
    }
}

impl I2c {
    /// Reads a register, with register address of type R and value of type V
    pub fn read_reg<R: AsBytes, V: Default + AsBytes + FromBytes>(
        &self,
        reg: R,
    ) -> Result<V, I2cError> {
        let mut val = V::default();

        let (code, _) = sys_send(
            self.task,
            Op::WriteRead as u16,
            &[self.address, self.interface as u8],
            &mut [],
            &[Lease::from(reg.as_bytes()), Lease::from(val.as_bytes_mut())],
        );

        if code != 0 {
            Err(I2cError::from(code))
        } else {
            Ok(val)
        }
    }
}
