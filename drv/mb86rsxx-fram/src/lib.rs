// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the Fujitsu MB85RS series of SPI FRAM chips.
//!
//! See
//! <https://www.mouser.com/datasheet/2/1113/MB85RS64T_DS501_00051_2v0_E-2329177.pdf>

#![no_std]

use drv_spi_api::{CsState, SpiDevice, SpiServer};
use ringbuf::ringbuf_entry;

pub type Mb86rs64t<S> = Fram<S, 8191>;

pub struct Fram<S: SpiServer, const SIZE: u16> {
    spi: SpiDevice<S>,
}

pub struct WritableFram<'fram, S: SpiServer, const SIZE: u16>(
    &'fram Fram<S, SIZE>,
);

#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
pub enum FramError {
    SpiError(drv_spi_api::SpiError),
    SpiServerDead,
    InvalidAddr,
    /// The write is longer than the highest address, would wrap around to the
    /// beginning of the FRAM!
    ///
    /// You probably don't want that.
    WouldWrap,
}

#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
#[repr(u8)]
enum Opcode {
    /// Set the write enable latch (WREN)
    SetWriteEn = 0b0000_0110,
    /// Reset the write enable latch (WRDI)
    ResetWriteEn = 0b0000_0100,
    /// Read the status register (RDSR)
    ReadStatus = 0b0000_0101,
    /// Write to the status register (WRSR)
    WriteStatus = 0b0000_0001,
    /// Read from memory (READ)
    Read = 0b0000_0011,
    /// Write to memory (WRITE)
    Write = 0b0000_0010,
    /// Read Device ID (RDID)
    ReadId = 0b1001_1111,
    /// Sleep mode (SLEEP)
    Sleep = 0b1011_1001,
    /// Reserved for future use (RFU).
    ///
    /// Probably don't send this command. You may regret this.
    Reserved = 0b0000_1011,
}

#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    Write {
        addr: u16,
        len: u16,
    },
    Written(#[count(children)] Result<(), FramError>),
    Read {
        addr: u16,
        len: u16,
    },
    // Unfortunately, the present and past tense of "read" are the same word,
    // because English is a very normal language. So, I've made up my own,
    // better past tense.
    Readed(#[count(children)] Result<(), FramError>),
    WriteEnable(#[count(children)] bool),
}

ringbuf::counted_ringbuf!(Trace, 16, Trace::None);

impl<S: SpiServer, const SIZE: u16> Fram<S, { SIZE }> {
    pub const fn new(spi: SpiDevice<S>) -> Self {
        Self { spi }
    }

    pub fn write_enable(&self) -> Result<(), FramError> {
        ringbuf_entry!(Trace::WriteEnable(true));
        self.spi.write(&[Opcode::SetWriteEn as u8])?;
        Ok(())
    }

    pub fn write_disable(&self) -> Result<(), FramError> {
        ringbuf_entry!(Trace::WriteEnable(false));
        self.spi.write(&[Opcode::ResetWriteEn as u8])?;
        Ok(())
    }

    pub fn with_write_enabled(&self) -> Result<WritableFram<'_>, FramError> {
        self.write_enable_raw()?;
        Ok(WritableFram(self))
    }

    pub fn write(&self, addr: u16, data: &[u8]) -> Result<(), FramError> {
        ringbuf_entry!(Trace::Write {
            addr,
            len: data.len() as u16
        });
        let result = self.actually_write(addr, data);
        ringbuf_entry!(Trace::Written(result));
        result
    }

    /// Actually do a write.
    ///
    /// This is a separate function just so we can look at the result that we
    /// get back and stick it in the ringbuf. If rustc doesn't inline this, I
    /// will be very sad.
    #[inline(always)]
    fn actually_write(&self, addr: u16, data: &[u8]) -> Result<(), FramError> {
        if addr > SIZE {
            return Err(FramError::InvalidAddr);
        }
        if addr as usize + data.len() > SIZE as usize {
            return Err(FramError::WouldWrap);
        }

        // The FRAM has a neat behavior where it can do multiple writes with
        // autoincrement for as long as CS remains low. If I understand the
        // datasheet correctly, this means that we can write the "write" opcode
        // followed by the start address and the first byte, and then continue
        // writing bytes to subsequent addresses without having to resend the
        // write command.
        //
        // This is, of course, contingent on my understanding the kind of
        // strangely worded datasheet text correctly (as is traditional):
        //
        // > The WRITE command writes data to FRAM memory cell array. WRITE
        // > op-code, arbitrary 16 bits of address and 8 bits of writing data
        // > are input to SI. The 3-bit upper address bit is invalid. When 8
        // > bits of writing data is input, data is written to FRAM memory cell
        // > array. Risen CS will terminate the WRITE command. However, if you
        // > continue sending the writing data for 8 bits each before CS rising,
        // > it is possible to continue writing with automatic address
        // > increment. When it reaches the most significant address, it rolls
        // > over to the starting address, and writing cycle keeps on continued
        // > infinitely.

        // Anyway, let's pull CS low.
        let lock = self
            .spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| FramError::SpiServerDead)?;
        // Write the `WRITE` command
        self.spi.write(&[Opcode::Write as u8])?;
        // Write address --- this is big-endian per the datasheet.
        self.spi.write(u16::to_be_bytes(addr))?;
        // Here is where we get to find out if the cool autoincrement thingy
        // actually works! We *should* be able to just keep squirting data bytes
        // at it as long as CS is held low, and it'll just do the right thing.
        self.spi.write(data)?;

        Ok(())
    }

    pub fn read(&self, addr: u16, data: &mut [u8]) -> Result<(), FramError> {
        ringbuf_entry!(Trace::Read {
            addr,
            len: data.len() as u16
        });
        let result = self.do_read(addr, data);
        ringbuf_entry!(Trace::Readed(result));
        result
    }

    /// Actually do a read.
    ///
    /// This is a separate function just so we can look at the result that we
    /// get back and stick it in the ringbuf. If rustc doesn't inline this, I
    /// will be very sad.
    #[inline(always)]
    fn actually_read(
        &self,
        addr: u16,
        data: &mut [u8],
    ) -> Result<(), FramError> {
        if addr > SIZE {
            return Err(FramError::InvalidAddr);
        }
        if addr as usize + data.len() > SIZE as usize {
            return Err(FramError::WouldWrap);
        }

        // Similarly to writes, the FRAM is supposed to auto-increment as long
        // as we keep clocking SCK with CS low. The datasheet says:
        //
        // > The READ command reads FRAM memory cell array data. Arbitrary 16
        // > bits address and op-code of READ are input to SI. The 3-bit upper
        // > address bit is invalid. Then, 8-cycle clock is input to SCK. SO is
        // > output synchronously to the falling edge of SCK. While reading, the
        // > SI value is invalid. When CS is risen, the READ command is
        // > completed, but keeps on reading with automatic address increment
        // > which is enabled by continuously sending clocks to SCK in unit of 8
        // > cycles before CS rising. When it reaches the most significant
        // > address, it rolls over to the starting address, and reading cycle
        // > keeps on infinitely

        // Anyway, let's pull CS low.
        let lock = self
            .spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| FramError::SpiServerDead)?;
        // Write the `READ` command
        self.spi.write(&[Opcode::Write as u8])?;
        // Write address --- this is big-endian per the datasheet.
        self.spi.write(u16::to_be_bytes(addr))?;
        // Now we ought to be able to keep reading until we've read the whole
        // buffer.
        self.spi.read(data)?;

        Ok(())
    }
}

impl From<drv_spi_api::SpiError> for FramError {
    fn from(value: drv_spi_api::SpiError) -> Self {
        FramError::SpiError(value)
    }
}

impl<S: SpiServer, const SIZE: usize> WritableFram<'_, S, { SIZE }> {
    pub fn write(&self, addr: u16, data: &[u8]) -> Result<(), FramError> {
        self.0.write(addr, data)
    }

    pub fn read(&self, addr: u16, data: &mut [u8]) -> Result<(), FramError> {
        self.0.read(addr, data)
    }
}

impl<S: SpiServer, const SIZE: usize> Drop for WritableFram<'_, S, { SIZE }> {
    fn drop(&mut self) {
        let _ = self.0.write_disable();
    }
}
