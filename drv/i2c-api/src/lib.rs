//! Client API for the I2C server

#![no_std]

use zerocopy::{AsBytes, FromBytes};

use userlib::*;

#[derive(FromPrimitive)]
pub enum Op {
    WriteRead = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
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

#[derive(Clone, Debug)]
pub struct I2c {
    task: TaskId,
    interface: Interface,
    address: u8,
}

impl I2c {
    pub fn new(task: TaskId, interface: Interface, address: u8) -> Self {
        Self { task: task, interface: interface, address: address }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum I2cError {
    Dead = !0,
    BadArg,
    NoDevice,
    Busy,
    BadInterface,
}

impl From<u32> for I2cError {
    fn from(x: u32) -> Self {
        match x {
            core::u32::MAX => I2cError::Dead,
            1 => I2cError::BadArg,
            2 => I2cError::NoDevice,
            3 => I2cError::Busy,
            4 => I2cError::BadInterface,
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
