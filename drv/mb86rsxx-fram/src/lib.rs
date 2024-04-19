// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the Fujitsu MB85RS series of SPI FRAM chips.
//!
//! See
//! <https://www.mouser.com/datasheet/2/1113/MB85RS64T_DS501_00051_2v0_E-2329177.pdf>

#![no_std]

use bitflags::bitflags;
use drv_spi_api::{CsState, SpiDevice, SpiError, SpiServer};
use num_traits::FromPrimitive;
use ringbuf::ringbuf_entry;

pub type Mb86rs64v<S> = Fram<S, { product_id::MB85RS64V }>;
pub type Mb86rs64t<S> = Fram<S, { product_id::MB85RS64T }>;
pub type Mb85rs256ty<S> = Fram<S, { product_id::MB85RS256TY }>;
pub type Mb85rs1mt<S> = Fram<S, { product_id::MB85RS1MT }>;
pub type Mb85rs2mta<S> = Fram<S, { product_id::MB85RS2MTA }>;
pub type Mb85rs4mt<S> = Fram<S, { product_id::MB85RS4MT }>;

/// A generic Fujitsu FRAM chip of arbitrary size.
///
/// By default, the write enable latch on the FRAM is not set, so this type
/// cannot be written to. To write to a FRAM chip, first call
/// [`Fram::write_enable`], which returns a [`WritableFram`].
#[must_use = "a Fram does nothing if constructed but not read from or written to"]
pub struct Fram<S: SpiServer, const ID: u16> {
    spi: SpiDevice<S>,
}

/// A generic Fujitsu FRAM chip with its write enable latch set.
///
/// This type is returned by [`Fram::write_enable`], and will unset the write
/// latch when it's dropped. This way, the FRAM remains in the write-protected
/// state when you're not actively trying to write to it.
///
/// To write to the FRAM chip, use [`WritableFram::write`]. This type also
/// exposes a [`WritableFram::read`] method, so the FRAM chip may also be read
/// from while the write enable latch is set.
#[must_use = "a WritableFram does nothing if constructed but not read from \
    or written to"]
pub struct WritableFram<'fram, S: SpiServer, const ID: u16>(
    &'fram Fram<S, { ID }>,
);

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct FramId {
    pub mfg_id: u8,
    pub product_id: u16,
}

#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
pub enum FramInitError {
    SpiError(#[count(children)] SpiError),
    UnknownManufacturerId(u8),
    UnexpectedProductId { expected: u16, actual: u16 },
}

/// Errors returned by the [`Fram::read`] and [`WritableFram::write`] methods.
#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
pub enum FramError {
    SpiError(#[count(children)] SpiError),
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

bitflags! {
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct Status: u8 {
        /// Write enable latch
        const WEL = 1 << 1;
        /// Block protect 0
        const BP0 = 1 << 2;
        /// Block protect 1
        const BP1 = 1 << 3;
        /// Status register write protect enabled
        const WPEN = 1 << 7;
    }
}

#[derive(Eq, PartialEq, Copy, Clone, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    ReadIdLow {
        mfg_id: u8,
        cont: u8,
    },
    ReadIdHigh {
        product_id: u16,
    },

    #[count(skip)]
    Status(Status),
    Writing {
        addr: usize,
        len: usize,
    },
    Wrote(#[count(children)] Result<(), FramError>),
    Reading {
        addr: usize,
        len: usize,
    },
    Read(#[count(children)] Result<(), FramError>),
    WriteEnable(#[count(children)] bool),
}

ringbuf::counted_ringbuf!(Trace, 16, Trace::None);

const KIB: usize = 1024;

impl<S: SpiServer, const ID: u16> Fram<S, { ID }> {
    /// The size in bytes of this FRAM chip.
    pub const SIZE: usize = product_id::size(ID);

    /// The highest address in this FRAM chip's address space.
    pub const MAX_ADDR: usize = Self::SIZE - 1;

    // How many bytes of address are significant when reading/writing to this FRAM?
    const NEEDS_THREE_BYTE_ADDRS: bool = Self::SIZE > 64 * KIB;

    pub fn new(spi: SpiDevice<S>) -> Result<Self, FramInitError> {
        // Look at the FRAM's ID to make sure it's the device we expect it to
        // be. In particular, make sure it's the size we think it is.
        let id = FramId::read(&spi).map_err(FramInitError::SpiError)?;
        if id.mfg_id != FramId::MANUFACTURER_FUJITSU {
            return Err(FramInitError::UnknownManufacturerId(id.mfg_id));
        }
        if id.product_id != ID {
            return Err(FramInitError::UnexpectedProductId {
                expected: ID,
                actual: id.product_id,
            });
        }

        Ok(Self { spi })
    }

    /// Reads the FRAM device's product ID.
    pub fn read_id(&self) -> Result<FramId, SpiError> {
        FramId::read(&self.spi)
    }

    /// Set the write enable latch, returning a [`WritableFram`] type that
    /// unsets the write enable latch when it's dropped. This way, the FRAM
    /// remains in the write-protected state unless you actually intend to write
    /// to it.
    pub fn write_enable(
        &self,
    ) -> Result<WritableFram<'_, S, { ID }>, SpiError> {
        self.do_write_enable()?;
        Ok(WritableFram(self))
    }

    /// Read bytes from the FRAM starting at `addr` into `buf`.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the read was successful.
    /// - [`Err`]`(`[`FramError::InvalidAddr`]`)` if the base address is larger
    ///   than the size of this FRAM chip ([`Self::SIZE`]).
    /// - [`Err`]`(`[`FramError::WouldWrap`)` if the *last* address to read from
    ///   (i.e. `addr + buf.len()`) is larger than the size of this FRAM chip
    ///   ([`Self::SIZE`]). would wrap around to the beginning of the FRAM. You
    ///   probably don't actually want that, so we won't let you do it.
    /// - [`Err`]`(`[`FramError::SpiError`]`)` if the SPI driver returned an
    ///   error.
    pub fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), FramError> {
        ringbuf_entry!(Trace::Reading {
            addr,
            len: buf.len(),
        });
        let result = self.actually_read(addr, buf);
        ringbuf_entry!(Trace::Read(result));
        result
    }

    /// Wham, bam, write to the FRAM!
    ///
    /// This is a separate function just so we can look at the result that we
    /// get back and stick it in the ringbuf. If rustc doesn't inline this, I
    /// will be very sad.
    #[inline(always)]
    fn actually_write(
        &self,
        addr: usize,
        data: &[u8],
    ) -> Result<(), FramError> {
        Self::bounds_check(addr, data.len())?;

        // The FRAM has a neat behavior where it can do multiple writes with
        // autoincrement for as long as CS remains low. This means that we can
        // write the "write" opcode followed by the start address and the first
        // byte, and then continue writing clocking in bytes to subsequent
        // addresses without  having to resend the write command or address.
        //
        // Here's the kind of strangely-worded explanation of this from the
        // Fujitsu datasheet:
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

        // Start the write command.
        let _cs_is_held_low_while_this_exists =
            self.start_rw_command(Opcode::Write, addr)?;
        // Actually write the data.
        self.spi.write(data)?;

        Ok(())
    }

    /// Actually do a read.
    ///
    /// This is a separate function just so we can look at the result that we
    /// get back and stick it in the ringbuf. If rustc doesn't inline this, I
    /// will be very sad.
    #[inline(always)]
    fn actually_read(
        &self,
        addr: usize,
        data: &mut [u8],
    ) -> Result<(), FramError> {
        Self::bounds_check(addr, data.len())?;

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

        // Start the read command.
        let _cs_is_held_low_while_this_exists =
            self.start_rw_command(Opcode::Read, addr)?;
        // Read until the buffer's full.
        self.spi.read(data)?;

        Ok(())
    }

    /// Starts a read or write command with an address to the FRAM, asserting CS
    /// and returning a lock that holds CS low.
    /// This assumes CS is held asserted.
    fn start_rw_command(
        &self,
        cmd: Opcode,
        addr: usize,
    ) -> Result<drv_spi_api::ControllerLock<'_, S>, FramError> {
        // Assert CS.
        let lock = self
            .spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| SpiError::TaskRestarted)?;

        // Write the base address. Depending on the size of the FRAM chip, this
        // may be a two- or three-byte address. The biggest FRAM chips use
        // three-byte addresses, so the first byte from `u32::to_be_bytes` is
        // always zero, and (depending on the size of the FRAM), the second byte
        // may also be zero. Thus, we decide which one to clobber based on the
        // FRAM size.
        //
        // This should (hopefully) always get const-folded.
        let cmd_idx: usize = if Self::NEEDS_THREE_BYTE_ADDRS { 0 } else { 1 };
        let mut buf = u32::to_be_bytes(addr as u32);
        buf[cmd_idx] = cmd as u8;
        self.spi.write(&buf[cmd_idx..])?;
        Ok(lock)
    }

    fn bounds_check(addr: usize, len: usize) -> Result<(), FramError> {
        if addr > Self::MAX_ADDR {
            return Err(FramError::InvalidAddr);
        }
        if addr + len > Self::MAX_ADDR {
            return Err(FramError::WouldWrap);
        }
        Ok(())
    }

    fn do_write_enable(&self) -> Result<(), SpiError> {
        ringbuf_entry!(Trace::WriteEnable(true));
        self.spi.write(&[Opcode::SetWriteEn as u8])?;
        Ok(())
    }

    fn do_write_disable(&self) -> Result<(), SpiError> {
        ringbuf_entry!(Trace::WriteEnable(false));
        self.spi.write(&[Opcode::ResetWriteEn as u8])?;
        Ok(())
    }
}

impl<S: SpiServer, const ID: u16> WritableFram<'_, S, { ID }> {
    /// Write bytes from `buf` to the FRAM, starting at `addr`.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the read was successful.
    /// - [`Err`]`(`[`FramError::InvalidAddr`]`)` if the base address is larger
    ///   than the size of this FRAM chip (`SIZE`).
    /// - [`Err`]`(`[`FramError::WouldWrap`)` if the *last* address to write to
    ///   (i.e. `addr + buf.len()`) is larger than the size of this FRAM chip
    ///   (`SIZE`). would wrap around to the beginning of the FRAM. You probably
    ///   don't actually want that, so we won't let you do it.
    /// - [`Err`]`(`[`FramError::SpiError`]`)` if the SPI driver returned an
    ///   error.
    pub fn write(&self, addr: usize, buf: &[u8]) -> Result<(), FramError> {
        ringbuf_entry!(Trace::Writing {
            addr,
            len: buf.len(),
        });
        let result = self.0.actually_write(addr, buf);
        ringbuf_entry!(Trace::Wrote(result));
        result
    }

    /// Read bytes from the FRAM starting at `addr` into `buf`.
    ///
    /// This is the same as [`Fram::read`], see the documentation for that
    /// method for details.
    pub fn read(&self, addr: usize, data: &mut [u8]) -> Result<(), FramError> {
        self.0.read(addr, data)
    }

    /// Reads the FRAM device's product ID.
    pub fn read_id(&self) -> Result<FramId, SpiError> {
        self.0.read_id()
    }

    pub fn write_disable(self) -> Result<(), SpiError> {
        self.0.do_write_disable()?;
        // Don't do it again.
        core::mem::forget(self);
        Ok(())
    }
}

impl FramId {
    const MANUFACTURER_FUJITSU: u8 = 0x04;

    fn read<S: SpiServer>(spi: &SpiDevice<S>) -> Result<Self, SpiError> {
        // Indicates that we must read another two bytes to get the product ID.
        const CONTINUE: u8 = 0x7f;

        let _cs_is_held_low_while_this_exists = spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| SpiError::TaskRestarted)?;
        let mut buf = [0; 3];
        spi.exchange(&[Opcode::ReadId as u8, 0, 0], &mut buf)?;
        let [_, mfg_id, cont] = buf;
        ringbuf_entry!(Trace::ReadIdLow { mfg_id, cont });

        let product_id = if cont == CONTINUE {
            let mut buf = [0; 2];
            spi.read(&mut buf)?;
            u16::from_be_bytes(buf)
        } else {
            // no continuation code --- use the bytes we just read.
            // AFAICT, the Fujitsu FRAM chips don't do this, but other
            // manufacturers' FRAM chips that use the same protocol do? Would
            // have to read some more datasheets to be sure.
            u16::from_be_bytes([mfg_id, cont])
        };

        ringbuf_entry!(Trace::ReadIdHigh { product_id });

        Ok(Self { mfg_id, product_id })
    }
}

impl<S: SpiServer, const ID: u16> Drop for WritableFram<'_, S, { ID }> {
    fn drop(&mut self) {
        // Put the FRAM back the way we found it.
        let _ = self.0.do_write_disable();
    }
}

impl From<SpiError> for FramError {
    fn from(value: SpiError) -> Self {
        FramError::SpiError(value)
    }
}

impl From<SpiError> for FramInitError {
    fn from(value: SpiError) -> Self {
        FramInitError::SpiError(value)
    }
}

pub mod product_id {
    /// 2kb Fujitsu FRAM
    pub const MB85RS16: u16 = 0x0101;
    /// 8kb Fujitsu FRAM
    pub const MB85RS64V: u16 = 0x0302;
    /// 8kb Fujitsu FRAM
    pub const MB85RS64T: u16 = 0x2303;
    /// 32kb Fujitsu FRAM
    pub const MB85RS256TY: u16 = 0x2503;
    /// 128kb Fujitsu FRAM
    pub const MB85RS1MT: u16 = 0x2703;
    /// 256kb Fujitsu FRAM
    pub const MB85RS2MTA: u16 = 0x4803;
    /// 512kb Fujitsu FRAM
    pub const MB85RS4MT: u16 = 0x4903;

    /// Returns the size in bytes of the FRAM chip, based on its product ID.
    pub(super) const fn size(product_id: u16) -> usize {
        // The first 5 bits of the product ID give the density of the FRAM chip
        // in multiples of 2KiB.
        //
        // For example, the 2kb MB85RS16 has the product ID 0x0101, so:
        //  0x01 & 0b0001_1111 = 1
        //  2^1 = 2
        //  2 * 1024 = 2048 bytes
        //
        // Or, for the 8kb MB85RS64V and MB85RS64T, which have product IDs
        // 0x0302 and 0x2303, respectively:
        //  0x03 & 0b0001_1111 = 3
        //  0x23 & 0b0001_1111 = 3
        //  2^3 = 8
        //  8 * 1024 * 8 = 8192 bytes
        const MASK: u8 = 0b0001_1111;
        let [hi, _] = u16::to_be_bytes(product_id);
        let density = hi & MASK;
        2usize.pow(density as u32) * super::KIB
    }
}
