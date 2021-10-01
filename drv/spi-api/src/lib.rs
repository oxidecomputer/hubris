//! Client API for the SPI server

#![no_std]

use userlib::*;

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
    /// Server has died
    Died = core::u32::MAX,

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
}

impl From<SpiError> for u32 {
    fn from(rc: SpiError) -> Self {
        rc as u32
    }
}

#[derive(Clone, Debug)]
pub struct Spi(pub TaskId);

impl Spi {
    /// Perform both a SPI write and a SPI read
    pub fn exchange(
        &self,
        source: &[u8],
        sink: &mut [u8],
    ) -> Result<(), SpiError> {
        let (code, _) = sys_send(
            self.0,
            Operation::Exchange as u16,
            &[],
            &mut [],
            &[Lease::from(source), Lease::from(sink)],
        );

        if code != 0 {
            Err(SpiError::from_u32(code).ok_or(SpiError::BadResponse)?)
        } else {
            Ok(())
        }
    }

    /// Perform a SPI write
    pub fn write(&self, source: &[u8]) -> Result<(), SpiError> {
        let (code, _) = sys_send(
            self.0,
            Operation::Write as u16,
            &[],
            &mut [],
            &[Lease::from(source)],
        );

        if code != 0 {
            Err(SpiError::from_u32(code).ok_or(SpiError::BadResponse)?)
        } else {
            Ok(())
        }
    }
}
