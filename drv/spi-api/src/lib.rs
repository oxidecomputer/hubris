//! Client API for the SPI server

#![no_std]

use userlib::*;
use core::cell::Cell;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
pub enum Operation {
    Read = 0b01,
    Write = 0b10,
    Exchange = 0b11,
}

impl Operation {
    pub fn is_read(self) -> bool {
        self as u32 & 1 != 0
    }

    pub fn is_write(self) -> bool {
        self as u32 & 0b10 != 0
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum SpiError {
    /// Malformed response
    BadResponse = 1,

    /// Bad argument
    BadArg = 2,

    /// Bad lease argument
    BadLeaseArg = 3,

    /// Bad lease attributes
    BadLeaseAttributes = 4,

    /// Bad source lease
    BadSource = 5,

    /// Bad source lease attibutes
    BadSourceAttributes = 6,

    /// Bad Sink lease
    BadSink = 7,

    /// Bad Sink lease attributes
    BadSinkAttributes = 8,

    /// Short sink length
    ShortSinkLength = 9,

    /// Bad lease count
    BadLeaseCount = 10,

    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 11,

    /// Could not transfer byte out of source
    BadSourceByte = 12,

    /// Could not transfer byte into sink
    BadSinkByte = 13,

    /// Server restarted
    ServerRestarted = 14,
}

impl From<SpiError> for u32 {
    fn from(rc: SpiError) -> Self {
        rc as u32
    }
}

#[derive(Clone, Debug)]
pub struct Spi(Cell<TaskId>);

impl From<TaskId> for Spi {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

impl Spi {
    fn result(&self, task: TaskId, code: u32) -> Result<(), SpiError> {
        if code != 0 {
            //
            // If we have an error code, check to see if it denotes a dearly
            // departed task; if it does, in addition to returning a specific
            // error code, we will set our task to be the new task as a courtesy.
            //
            if let Some(g) = abi::extract_new_generation(code) {
                self.0.set(TaskId::for_index_and_gen(task.index(), g));
                Err(SpiError::ServerRestarted)
            } else {
                Err(SpiError::from_u32(code).ok_or(SpiError::BadResponse)?)
            }
        } else {
            Ok(())
        }
    }

    /// Perform both a SPI write and a SPI read
    pub fn exchange(
        &self,
        source: &[u8],
        sink: &mut [u8],
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            task,
            Operation::Exchange as u16,
            &[],
            &mut [],
            &[Lease::from(source), Lease::from(sink)],
        );

        self.result(task, code)
    }

    /// Perform a SPI write
    pub fn write(
        &self,
        source: &[u8],
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            self.0.get(),
            Operation::Write as u16,
            &[],
            &mut [],
            &[Lease::from(source)],
        );

        self.result(task, code)
    }
}
