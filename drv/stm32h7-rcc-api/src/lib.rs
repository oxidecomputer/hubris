//! Client API for the STM32H7 RCC server.

#![no_std]

use byteorder::LittleEndian;
use zerocopy::{AsBytes, U32};

use userlib::*;

enum Op {
    EnableClock = 1,
    DisableClock = 2,
    EnterReset = 3,
    LeaveReset = 4,
}

#[derive(Clone, Debug)]
pub struct Rcc(TaskId);

impl From<TaskId> for Rcc {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

#[derive(Copy, Clone, Debug)]
pub enum RccError {
    Dead = !0,
}

impl From<u32> for RccError {
    fn from(x: u32) -> Self {
        match x {
            core::u32::MAX => RccError::Dead,
            _ => panic!(),
        }
    }
}

impl Rcc {
    pub fn enable_clock_raw(&self, index: usize) -> Result<(), RccError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Request(U32<LittleEndian>);

        impl hl::Call for Request {
            const OP: u16 = Op::EnableClock as u16;
            type Response = ();
            type Err = RccError;
        }

        hl::send(self.0, &Request(U32::new(index as u32)))
    }

    pub fn disable_clock_raw(&self, index: usize) -> Result<(), RccError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Request(U32<LittleEndian>);

        impl hl::Call for Request {
            const OP: u16 = Op::DisableClock as u16;
            type Response = ();
            type Err = RccError;
        }

        hl::send(self.0, &Request(U32::new(index as u32)))
    }

    pub fn enter_reset_raw(&self, index: usize) -> Result<(), RccError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Request(U32<LittleEndian>);

        impl hl::Call for Request {
            const OP: u16 = Op::EnterReset as u16;
            type Response = ();
            type Err = RccError;
        }

        hl::send(self.0, &Request(U32::new(index as u32)))
    }

    pub fn leave_reset_raw(&self, index: usize) -> Result<(), RccError> {
        #[derive(AsBytes)]
        #[repr(C)]
        struct Request(U32<LittleEndian>);

        impl hl::Call for Request {
            const OP: u16 = Op::LeaveReset as u16;
            type Response = ();
            type Err = RccError;
        }

        hl::send(self.0, &Request(U32::new(index as u32)))
    }
}
