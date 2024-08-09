// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver for FMC-attached NOR flash

#![no_std]
#![no_main]

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
    SectorEraseBusy,
    WriteBusy,
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
        unsafe {
            reg::DATA_BYTES.write_volatile(dest.len() as u32);
            reg::ADDR.write_volatile(offset);
            reg::DUMMY_CYCLES.write_volatile(0);
            reg::INSTR.write_volatile(instr::READ);
            self.wait_fpga_busy();
            for i in 0..dest.len().div_ceil(4) {
                let v = reg::RX_FIFO.read_volatile().to_le_bytes();
                for j in 0..4 {
                    let k = i * 4 + j;
                    if k < dest.len() {
                        dest.write_at(k, v[j])
                            .map_err(|_| RequestError::went_away())?;
                    }
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

        self.write_enable();
        unsafe {
            reg::DATA_BYTES.write_volatile(0);
            reg::ADDR.write_volatile(offset);
            reg::DUMMY_CYCLES.write_volatile(0);
            reg::INSTR.write_volatile(instr::SECTOR_ERASE);
        }
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

        self.write_enable();
        unsafe {
            reg::DATA_BYTES.write_volatile(data.len() as u32);
            reg::ADDR.write_volatile(offset);
            reg::DUMMY_CYCLES.write_volatile(0);
            for i in 0..data.len().div_ceil(4) {
                let mut v = [0u8; 4];
                for j in 0..4 {
                    let k = i * 4 + j;
                    if k < data.len() {
                        if let Some(b) = data.read_at(i * 4 + j) {
                            v[j] = b;
                        } else {
                            return Err(RequestError::went_away());
                        }
                    }
                }
                reg::TX_FIFO.write_volatile(u32::from_le_bytes(v));
            }
            reg::INSTR.write_volatile(instr::PAGE_PROGRAM);
            self.wait_fpga_busy();
        }

        // Wait for the busy flag to be unset
        self.wait_flash_busy(Trace::WriteBusy);
        Ok(())
    }
}

impl ServerImpl {
    /// Reads the STATUS_1 register from the SPI flash
    fn read_status(&self) -> u8 {
        unsafe {
            reg::DATA_BYTES.write_volatile(1);
            reg::ADDR.write_volatile(0);
            reg::DUMMY_CYCLES.write_volatile(0);
            reg::INSTR.write_volatile(instr::READ_STATUS_1);
            self.wait_fpga_busy();
            reg::RX_FIFO.read_volatile().to_le_bytes()[0]
        }
    }

    /// Sets the write enable flag in the SPI flash
    fn write_enable(&self) {
        unsafe {
            reg::DATA_BYTES.write_volatile(0);
            reg::ADDR.write_volatile(0);
            reg::DUMMY_CYCLES.write_volatile(0);
            reg::INSTR.write_volatile(instr::WRITE_ENABLE);
            self.wait_fpga_busy();
        }
    }

    /// Wait until the FPGA is idle
    fn wait_fpga_busy(&self) {
        unsafe {
            let status = reg::SPISR.read_volatile();
            while (status & 1) != 0 {
                ringbuf_entry!(Trace::FpgaBusy(status));
                sleep_for(1);
            }
        }
    }

    /// Wait until the SPI flash is idle
    fn wait_flash_busy(&self, t: Trace) {
        // Wait for the busy flag to be unset
        while (self.read_status() & 1) != 0 {
            ringbuf_entry!(t);
            sleep_for(1);
        }
    }
}

mod idl {
    use super::NorFlashError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
