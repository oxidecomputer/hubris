//! Sequencer FPGA SPI+GPIO communication driver.
//!
//! This uses external shared SPI and GPIO servers to drive the FPGA.

use zerocopy::{AsBytes, Unaligned, U16};

use drv_spi_api as spi_api;
use drv_stm32h7_gpio_api as gpio_api;

#[derive(AsBytes, Unaligned)]
#[repr(u8)]
enum Cmd {
    Write = 0,
    Read = 1,
    BitSet = 2,
    BitClear = 3,
    //Identify = 4, // proposed in RFD; implemented?
}

pub enum Addr {
    Id0 = 0,
    InterruptFlag = 4,
    InterruptEnable = 5,
    SequencerStatus = 6,
    PowerControl = 7,
    FanFeedback = 8,
    A1Status = 9,
    A1PowerReadback = 0xa,
    A0PowerReadback = 0xb,
    A0PowerGoodGroupB = 0xc,
    A0PowerGoodUnused = 0xd,
    A0PowerFaultAPlusB = 0xe,
    A0StatusGroupC = 0xf,
    FanOutStatus = 0x10,
    A1OutputStatus = 0x11,
    A1OutputDebug = 0x12,
}

impl From<Addr> for u16 {
    fn from(a: Addr) -> Self {
        a as u16
    }
}

pub const EXPECTED_IDENT: u32 = 0x1DE_AA55;

pub struct SequencerFpga {
    spi: spi_api::SpiDevice,
}

impl SequencerFpga {
    pub fn new(spi: spi_api::SpiDevice) -> Self {
        Self { spi }
    }

    /// Reads the IDENT0:3 registers as a big-endian 32-bit integer.
    pub fn read_ident(
        &self,
    ) -> Result<u32, spi_api::SpiError> {
        let mut ident = 0;
        self.read_bytes(Addr::Id0, ident.as_bytes_mut())?;
        Ok(ident)
    }

    /// Performs the READ command against `addr`. This can read as many bytes as
    /// you like into `data_out`.
    pub fn read_bytes(
        &self,
        addr: impl Into<u16>,
        data_out: &mut [u8],
    ) -> Result<(), spi_api::SpiError> {
        self.raw_spi_read(Cmd::Read, addr.into(), data_out)
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
        let mut data = [ 0u8; 16 ];
        let mut rval = [ 0u8; 16 ];

        let addr = U16::new(addr);
        let header = CmdHeader { cmd, addr };
        let header = header.as_bytes();

        for i in 0..header.len() {
            data[i] = header[i];
        }

        self.spi.exchange(&data, &mut rval)?;

        for i in 0..data_out.len() {
            if i + header.len() < data.len() {
                data_out[i] = rval[i + header.len()];
            }
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
        let mut data = [ 0u8; 16 ];
        let mut rval = [ 0u8; 16 ];

        let addr = U16::new(addr);
        let header = CmdHeader { cmd, addr };
        let header = header.as_bytes();

        for i in 0..header.len() {
            data[i] = header[i];
        }

        for i in 0..data_in.len() {
            if i + header.len() < data.len() {
                data[i + header.len()] = data_in[i];
            }
        }

        self.spi.exchange(&data, &mut rval)?;

        Ok(())
    }
}

#[derive(AsBytes, Unaligned)]
#[repr(C)]
struct CmdHeader {
    cmd: Cmd,
    addr: U16<byteorder::BigEndian>,
}
