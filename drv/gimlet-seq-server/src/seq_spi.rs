//! Sequencer FPGA SPI+GPIO communication driver.
//!
//! This uses external shared SPI and GPIO servers to drive the FPGA.

use zerocopy::{AsBytes, Unaligned, U16};

use drv_spi_api as spi_api;
use drv_stm32h7_gpio_api as gpio_api;

#[derive(AsBytes, Unaligned)]
#[repr(u8)]
pub enum Cmd {
    Write = 0,
    Read = 1,
    BitSet = 2,
    BitClear = 3,
    //Identify = 4, // proposed in RFD; implemented?
}

pub enum Addr {
    Id0 = 0,

    A1SmStatus = 10,
    A0SmStatus = 11,
    EarlyRbks = 12,
    A1Readbacks = 13,
    AmdA0 = 14,
    GroupBPg = 15,
    GroupBUnused = 16,
    GroupBCFlts = 17,
    GroupCPg = 18,
    NicStatus = 19,
    ClkgenStatus = 20,
    AmdStatus = 21,
    FanOutStatus = 22,
    EarlyPwrStatus = 23,
    A1OutStatus = 24,
    A0OutStatus1 = 25,
    A0OutStatus2 = 26,
    OutStatusNic1 = 27,
    OutStatusNic2 = 28,
    ClkgenOutStatus = 29,
    AmdOutStatus = 30,
}

impl From<Addr> for u16 {
    fn from(a: Addr) -> Self {
        a as u16
    }
}

pub const EXPECTED_IDENT: u32 = 0x1DE_AA55;

pub struct SequencerFpga {
    spi: spi_api::SpiDevice,
    gpio: gpio_api::Gpio,
}

impl SequencerFpga {
    pub fn new(spi: spi_api::SpiDevice, gpio: gpio_api::Gpio) -> Self {
        Self { spi, gpio }
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
        // The current SPI API doesn't let us do "scatter-gather" style
        // write-read sequences, which is kind of what we want to construct a
        // SPI read command against the FPGA without allocating huge buffers.
        // (TODO: we should change this eventually.)
        //
        // Instead, we issue several transactions while keeping CS asserted
        // using the SPI lock facility.
        let _lock = self.spi.lock_auto(spi_api::CsState::Asserted)?;

        let addr = U16::new(addr);
        self.spi.write(CmdHeader { cmd, addr }.as_bytes())?;
        self.spi.read(data_out)?;
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
        // While writes are in theory easier than reads with the current SPI
        // API, in practice, we still need to do a "gather" and prepend header
        // data to `data_in`, so, we still have to lock.
        let _lock = self.spi.lock_auto(spi_api::CsState::Asserted)?;

        let addr = U16::new(addr);
        self.spi.write(CmdHeader { cmd, addr }.as_bytes())?;
        self.spi.write(data_in)?;
        Ok(())
    }
}

#[derive(AsBytes, Unaligned)]
#[repr(C)]
struct CmdHeader {
    cmd: Cmd,
    addr: U16<byteorder::BigEndian>,
}
