//! Client API for the I2C server

#![no_std]

use byteorder::LittleEndian;
use zerocopy::{AsBytes, FromBytes};

use userlib::*;

enum Op {
    WriteRead = 1,
}

#[derive(Copy, Clone, Debug)]
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
pub struct I2c(TaskId);

impl From<TaskId> for I2c {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum I2cError {
    Dead = !0,
    BadArg,
    NoDevice,
    Busy,
}

impl From<u32> for I2cError {
    fn from(x: u32) -> Self {
        match x {
            core::u32::MAX => I2cError::Dead,
            1 => I2cError::BadArg,
            2 => I2cError::NoDevice,
            3 => I2cError::Busy,
            _ => panic!(),
        }
    }
}

impl I2c {
    /// Reads a register, with register address of type R and value of type V
    pub fn read_reg<R: AsBytes, V: Default + AsBytes + FromBytes>(
        &self,
        interface: Interface,
        address: u8,
        reg: R,
    ) -> Result<V, I2cError> {
        let mut val = V::default();

        let (code, _) = sys_send(
            self.0,
            Op::WriteRead as u16,
            &[address],
            &mut [],
            &[ Lease::from(reg.as_bytes()), Lease::from(val.as_bytes_mut()) ],
        );

        if code != 0 {
            Err(I2cError::from(code))
        } else {
            Ok(val)
        }
    }
}
