// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sequencer FPGA SPI+GPIO communication driver.
//!
//! This uses external shared SPI and GPIO servers to drive the FPGA.

use zerocopy::{byteorder::big_endian as be, Immutable, IntoBytes, Unaligned};

use drv_spi_api as spi_api;
use spi_api::{SpiDevice, SpiServer};

#[derive(IntoBytes, Unaligned, Immutable)]
#[repr(u8)]
pub enum Cmd {
    Write = 0,
    Read = 1,
    BitSet = 2,
    BitClear = 3,
}

include!(env!("GIMLET_FPGA_REGS"));

pub const EXPECTED_IDENT: u16 = 0x1DE;

/// Local buffer size for chunked reads and writes
const RAW_SPI_BUFFER_SIZE: usize = 16;

/// Available space in the chunked read/write buffer for user data
#[allow(unused)] // only used in static assertions, which are compiled out
pub const MAX_SPI_CHUNK_SIZE: usize =
    RAW_SPI_BUFFER_SIZE - core::mem::size_of::<CmdHeader>();

pub struct SequencerFpga<S: SpiServer> {
    spi: SpiDevice<S>,
}

impl<S: SpiServer> SequencerFpga<S> {
    pub fn new(spi: SpiDevice<S>) -> Self {
        Self { spi }
    }

    /// Reads the IDENT0:1 registers as a big-endian 16-bit integer.
    pub fn read_ident(&self) -> Result<u16, spi_api::SpiError> {
        let mut ident = 0;
        self.read_bytes(Addr::ID0, ident.as_mut_bytes())?;
        Ok(ident)
    }

    /// Check for a valid identifier, deliberately eating any SPI errors.
    pub fn valid_ident(&self) -> bool {
        if let Ok(ident) = self.read_ident() {
            ident == EXPECTED_IDENT
        } else {
            false
        }
    }

    /// Reads the 32-bit checksum register, which should match
    /// `GIMLET_BITSTREAM_CHECKSUM` if the image is loaded and hasn't changed.
    pub fn read_checksum(&self) -> Result<u32, spi_api::SpiError> {
        let mut checksum = 0;
        self.read_bytes(Addr::CS0, checksum.as_mut_bytes())?;
        Ok(checksum)
    }

    /// Writes the 32-bit checksum to match `GIMLET_BITSTREAM_CHECKSUM`.
    ///
    /// This should be done after the image is loaded, to record the image's
    /// identity; if the Hubris image is power-cycled, this lets us detect
    /// whether the FPGA should be reloaded.
    pub fn write_checksum(&self) -> Result<(), spi_api::SpiError> {
        self.write_bytes(Addr::CS0, GIMLET_BITSTREAM_CHECKSUM.as_bytes())
    }

    /// Check for a valid checksum, deliberately eating any SPI errors.
    pub fn valid_checksum(&self) -> bool {
        if let Ok(checksum) = self.read_checksum() {
            checksum == GIMLET_BITSTREAM_CHECKSUM
        } else {
            false
        }
    }

    /// Performs the READ command against `addr`. This can read as many bytes as
    /// you like into `data_out`, limited by `raw_spi_read` buffer size
    pub fn read_bytes(
        &self,
        addr: impl Into<u16>,
        data_out: &mut [u8],
    ) -> Result<(), spi_api::SpiError> {
        self.raw_spi_read(Cmd::Read, addr.into(), data_out)
    }

    /// Performs a single-byte READ command against `addr` as a convenience
    /// routine
    pub fn read_byte(
        &self,
        addr: impl Into<u16>,
    ) -> Result<u8, spi_api::SpiError> {
        let mut buf = [0u8];
        self.read_bytes(addr, &mut buf)?;
        Ok(buf[0])
    }

    /// Performs the WRITE command against `addr`. This can write as many bytes
    /// as you like from `data_in`.
    pub fn write_bytes(
        &self,
        addr: impl Into<u16>,
        data_in: &[u8],
    ) -> Result<(), spi_api::SpiError> {
        self.raw_spi_write(Cmd::Write, addr.into(), data_in)
    }

    /// Performs the BITSET command against `addr`. This will bitwise-OR
    /// `data_in` with the target contents.
    pub fn set_bytes(
        &self,
        addr: impl Into<u16>,
        data_in: &[u8],
    ) -> Result<(), spi_api::SpiError> {
        self.raw_spi_write(Cmd::BitSet, addr.into(), data_in)
    }

    /// Performs the BITCLR command against `addr`. This will bitwise-AND
    /// the target contents with the _complement_ of `data_in`.
    pub fn clear_bytes(
        &self,
        addr: impl Into<u16>,
        data_in: &[u8],
    ) -> Result<(), spi_api::SpiError> {
        self.raw_spi_write(Cmd::BitClear, addr.into(), data_in)
    }

    /// Performs a read-shaped transaction using an arbitrary command and any
    /// address. It's important that `cmd` is one that ignores data sent by us
    /// after the address, or this will overwrite `addr` with arbitrary data.
    pub fn raw_spi_read(
        &self,
        cmd: Cmd,
        addr: u16,
        data_out: &mut [u8],
    ) -> Result<(), spi_api::SpiError> {
        let mut data = [0u8; RAW_SPI_BUFFER_SIZE];
        let mut rval = [0u8; RAW_SPI_BUFFER_SIZE];

        let addr = be::U16::new(addr);
        let header = CmdHeader { cmd, addr };
        let header = header.as_bytes();

        if data_out.len() > MAX_SPI_CHUNK_SIZE {
            return Err(spi_api::SpiError::BadTransferSize);
        }

        data[..header.len()].copy_from_slice(header);

        self.spi.exchange(&data, &mut rval)?;

        for i in 0..data_out.len() {
            data_out[i] = rval[i + header.len()];
        }

        Ok(())
    }

    /// Performs a write-shaped transaction using an arbitrary command and any
    /// address.
    pub fn raw_spi_write(
        &self,
        cmd: Cmd,
        addr: u16,
        data_in: &[u8],
    ) -> Result<(), spi_api::SpiError> {
        let mut data = [0u8; RAW_SPI_BUFFER_SIZE];
        let mut rval = [0u8; RAW_SPI_BUFFER_SIZE];

        let addr = be::U16::new(addr);
        let header = CmdHeader { cmd, addr };
        let header = header.as_bytes();

        data[..header.len()].copy_from_slice(header);

        for i in 0..data_in.len() {
            if i + header.len() < data.len() {
                data[i + header.len()] = data_in[i];
            }
        }

        self.spi.exchange(&data, &mut rval)?;

        Ok(())
    }
}

#[derive(IntoBytes, Unaligned, Immutable)]
#[repr(C)]
struct CmdHeader {
    cmd: Cmd,
    addr: be::U16,
}
