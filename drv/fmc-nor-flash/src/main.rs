// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver for FMC-attached NOR flash

#![no_std]
#![no_main]

use core::convert::Infallible;
use userlib::{hl::sleep_for, *};

use derive_idol_err::IdolError;
use idol_runtime::{Leased, LenLimit, NotificationHandler, RequestError, R, W};
use ringbuf::{counted_ringbuf, ringbuf_entry};

#[derive(Copy, Clone, Debug, FromPrimitive, IdolError, counters::Count)]
pub enum NorFlashError {
    /// The data write address is not at a page boundary
    NotPageAligned = 1,

    /// The sector address is not at a sector boundary
    NotSectorAligned,

    /// The data to be written is not a complete page
    NotFullPage,

    #[idol(server_death)]
    ServerRestarted,
}

#[derive(Debug, Clone, Copy, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    FpgaBusy(u32),
    FlashStatus(u8),
    SectorEraseBusy,
    WriteBusy,
    WriteWord(u32),
    ReadWord(u32),

    Lol(u32),
}

counted_ringbuf!(Trace, 32, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    // Wait for the FMC to be configured
    userlib::hl::sleep_for(1000);

    let id = unsafe { reg::BASE.read_volatile() };
    assert_eq!(id, 0x1de);

    // Fire up a server.
    let mut server = ServerImpl;
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl;

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[allow(unused)]
mod reg {
    pub const BASE: *mut u32 = 0x60000000 as *mut _;

    pub const NOR: *mut u32 = BASE.wrapping_add(0x40);
    pub const SPICR: *mut u32 = NOR.wrapping_add(0x0);
    pub const SPISR: *mut u32 = NOR.wrapping_add(0x1);
    pub const ADDR: *mut u32 = NOR.wrapping_add(0x2);
    pub const DUMMY_CYCLES: *mut u32 = NOR.wrapping_add(0x3);
    pub const DATA_BYTES: *mut u32 = NOR.wrapping_add(0x4);
    pub const INSTR: *mut u32 = NOR.wrapping_add(0x5);
    pub const TX_FIFO: *mut u32 = NOR.wrapping_add(0x6);
    pub const RX_FIFO: *mut u32 = NOR.wrapping_add(0x7);
}

mod instr {
    pub const PAGE_PROGRAM: u32 = 0x02;
    pub const READ: u32 = 0x03;
    pub const READ_STATUS_1: u32 = 0x05;
    pub const READ_STATUS_3: u32 = 0x15;
    pub const WRITE_ENABLE: u32 = 0x06;
    pub const SECTOR_ERASE: u32 = 0x20;
}

impl idl::InOrderFmcNorFlashImpl for ServerImpl {
    fn read(
        &mut self,
        _: &RecvMessage,
        offset: u32,
        dest: LenLimit<Leased<W, [u8]>, 256>,
    ) -> Result<(), RequestError<NorFlashError>> {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, dest.len() as u32);
        self.write_reg(reg::ADDR, offset);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ);
        self.wait_fpga_busy();
        for i in 0..dest.len().div_ceil(4) {
            let v = self.read_reg(reg::RX_FIFO);
            ringbuf_entry!(Trace::ReadWord(v));
            let v = v.to_le_bytes();
            for (j, byte) in v.iter().enumerate() {
                let k = i * 4 + j;
                if k < dest.len() {
                    dest.write_at(k, *byte)
                        .map_err(|_| RequestError::went_away())?;
                }
            }
        }
        Ok(())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        offset: u32,
    ) -> Result<(), RequestError<NorFlashError>> {
        if offset % 4096 != 0 {
            return Err(NorFlashError::NotSectorAligned.into());
        }

        ServerImpl::flash_write_enable(self);
        self.write_reg(reg::DATA_BYTES, 0);
        self.write_reg(reg::ADDR, offset);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::SECTOR_ERASE);
        // Wait for the busy flag to be unset
        self.wait_flash_busy(Trace::SectorEraseBusy);
        Ok(())
    }

    fn page_write(
        &mut self,
        _: &RecvMessage,
        offset: u32,
        data: LenLimit<Leased<R, [u8]>, 256>,
    ) -> Result<(), RequestError<NorFlashError>> {
        if offset % 256 != 0 {
            return Err(NorFlashError::NotPageAligned.into());
        } else if data.len() != 256 {
            return Err(NorFlashError::NotFullPage.into());
        }

        ServerImpl::flash_write_enable(self);
        let status = ServerImpl::read_flash_status_3(self);
        ringbuf_entry!(Trace::FlashStatus(status));
        self.write_reg(reg::DATA_BYTES, data.len() as u32);
        self.write_reg(reg::ADDR, offset);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        for i in 0..data.len().div_ceil(4) {
            let mut v = [0u8; 4];
            for (j, byte) in v.iter_mut().enumerate() {
                let k = i * 4 + j;
                if k < data.len() {
                    if let Some(b) = data.read_at(k) {
                        *byte = b;
                    } else {
                        return Err(RequestError::went_away());
                    }
                }
            }
            ringbuf_entry!(Trace::WriteWord(u32::from_le_bytes(v)));
            self.write_reg(reg::TX_FIFO, u32::from_le_bytes(v));
        }
        self.write_reg(reg::INSTR, instr::PAGE_PROGRAM);
        self.wait_fpga_busy();

        // Wait for the busy flag to be unset
        self.wait_flash_busy(Trace::WriteBusy);
        Ok(())
    }

    /// Reads the STATUS_1 register from the SPI flash
    fn read_flash_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<Infallible>> {
        Ok(ServerImpl::read_flash_status(self))
    }

    /// Sets the write enable flag in the SPI flash
    fn flash_write_enable(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        ServerImpl::flash_write_enable(self);
        Ok(())
    }

    fn write_test(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        self.clear_fifos();
        unsafe {
            // Do Write enable
            (0x60000108 as *mut u32).write_volatile(0x00);
            sleep_for(100);
            (0x6000010c as *mut u32).write_volatile(0x00);
            sleep_for(100);
            (0x60000110 as *mut u32).write_volatile(0x00);
            sleep_for(100);
            (0x60000114 as *mut u32).write_volatile(0x06);
            sleep_for(100);

            // Write to tx fifo
            (0x60000118 as *mut u32).write_volatile(0x03020100);
            sleep_for(100);
            (0x60000118 as *mut u32).write_volatile(0x07060504);
            sleep_for(100);

            // Show 8 bytes in tx fifo
            ringbuf_entry!(Trace::Lol(
                (0x60000104 as *mut u32).read_volatile()
            ));
            sleep_for(100);
            // Page write to flash
            (0x60000110 as *mut u32).write_volatile(0x08);
            sleep_for(100);
            (0x60000114 as *mut u32).write_volatile(0x02);
            sleep_for(100);

            // Show empty tx fifo
            ringbuf_entry!(Trace::Lol(
                (0x60000104 as *mut u32).read_volatile()
            ));
            sleep_for(100);

            // page read from flash
            (0x60000108 as *mut u32).write_volatile(0x00);
            sleep_for(100);
            (0x60000110 as *mut u32).write_volatile(0x08);
            sleep_for(100);
            (0x60000114 as *mut u32).write_volatile(0x03);
            sleep_for(100);

            // Show non-empty read fifo
            ringbuf_entry!(Trace::Lol(
                (0x60000104 as *mut u32).read_volatile()
            ));
            sleep_for(100);

            // read from fifo
            ringbuf_entry!(Trace::Lol(
                (0x6000011c as *mut u32).read_volatile()
            ));
            sleep_for(100);

            ringbuf_entry!(Trace::Lol(
                (0x6000011c as *mut u32).read_volatile()
            ));
            sleep_for(100);

            // Show empty tx fifo
            ringbuf_entry!(Trace::Lol(
                (0x60000104 as *mut u32).read_volatile()
            ));
            sleep_for(100);
        }
        Ok(())
    }
}

impl ServerImpl {
    fn read_reg(&self, reg: *mut u32) -> u32 {
        unsafe { reg.read_volatile() }
    }

    fn write_reg(&self, reg: *mut u32, v: u32) {
        unsafe { reg.write_volatile(v) };
    }

    /// Wait until the FPGA is idle
    fn wait_fpga_busy(&self) {
        loop {
            let status = self.read_reg(reg::SPISR);
            if (status & 1) == 0 {
                break;
            }
            ringbuf_entry!(Trace::FpgaBusy(status));
            sleep_for(1);
        }
    }

    /// Wait until the SPI flash is idle
    fn wait_flash_busy(&self, t: Trace) {
        // Wait for the busy flag to be unset
        while (self.read_flash_status() & 1) != 0 {
            ringbuf_entry!(t);
            sleep_for(1);
        }
    }

    fn read_flash_status(&self) -> u8 {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 1);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ_STATUS_1);
        self.wait_fpga_busy();
        self.read_reg(reg::RX_FIFO).to_le_bytes()[0]
    }

    fn read_flash_status_3(&self) -> u8 {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 1);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ_STATUS_3);
        self.wait_fpga_busy();
        self.read_reg(reg::RX_FIFO).to_le_bytes()[0]
    }

    fn flash_write_enable(&self) {
        self.write_reg(reg::DATA_BYTES, 0);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::WRITE_ENABLE);
        self.wait_fpga_busy();
    }

    fn clear_fifos(&self) {
        self.write_reg(reg::SPICR, 0x8080);
    }
}

mod idl {
    use super::NorFlashError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
