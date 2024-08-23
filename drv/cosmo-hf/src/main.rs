// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver for FMC-attached NOR flash, implementing the `hf` API
//!
//! The NOR flash chip is a W25Q01JVZEIQ, which is a 1 GiB NOR flash.  It is
//! connected to the FPGA over SPI / QSPI.
//!
//! # References
//! - [SPI NOR controller](https://github.com/oxidecomputer/quartz/tree/main/hdl/ip/vhd/spi_nor_controller/docs)

#![no_std]
#![no_main]

use ringbuf::{counted_ringbuf, ringbuf_entry};
use userlib::{hl::sleep_for, task_slot};

mod hf; // implementation of `HostFlash` API

task_slot!(SEQ, grapefruit_seq);

#[derive(Debug, Clone, Copy, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    FpgaBusy {
        spi_sr: u32,
    },
    SectorEraseBusy,
    WriteBusy,

    HashInitError(drv_hash_api::HashError),
    HashUpdateError(drv_hash_api::HashError),
    HashFinalizeError(drv_hash_api::HashError),
}

counted_ringbuf!(Trace, 32, Trace::None);

/// Size in bytes of a single page of data (i.e., the max length of slice we
/// accept for `page_program()` and `read_memory()`).
///
/// This value is really a property of the flash we're talking to and not this
/// driver, but it's correct for all our current parts. If that changes, this
/// will need to change to something more flexible.
pub const PAGE_SIZE_BYTES: usize = 256;

/// Size in bytes of a single sector of data (i.e., the size of the data erased
/// by a call to `sector_erase()`).
///
/// This value is really a property of the flash we're talking to and not this
/// driver, but it's correct for all our current parts. If that changes, this
/// will need to change to something more flexible.
///
/// **Note:** the datasheet refers to a "sector" as a 4K block, but also
/// supports 64K block erases, so we call the latter a sector to match the
/// behavior of the Gimlet host flash driver.
pub const SECTOR_SIZE_BYTES: u32 = 65_536;

#[export_name = "main"]
fn main() -> ! {
    // Wait for the FPGA to be configured
    let seq = drv_grapefruit_seq_api::Sequencer::from(SEQ.get_task_id());
    seq.ping(); // waits until the sequencer has completed configuration

    let id = unsafe { reg::BASE.read_volatile() };
    if id != 0x1de {
        fail(drv_hf_api::HfError::FpgaNotConfigured);
    }

    let drv = FlashDriver;
    drv.flash_set_quad_enable();

    // Check the flash chip's ID against Table 7.3.1 in the datasheet
    let id = drv.flash_read_id();
    if id[0..3] != [0xef, 0x40, 0x21] {
        fail(drv_hf_api::HfError::BadChipId);
    }

    let mut server = hf::ServerImpl { drv };

    let mut buffer = [0; hf::idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

/// Driver for a QSPI NOR flash controlled by an FPGA over FMC
struct FlashDriver;

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

#[allow(unused)]
mod instr {
    pub const PAGE_PROGRAM: u32 = 0x02;
    pub const READ: u32 = 0x03;
    pub const READ_STATUS_1: u32 = 0x05;
    pub const READ_STATUS_2: u32 = 0x35;
    pub const READ_STATUS_3: u32 = 0x15;
    pub const WRITE_STATUS_2: u32 = 0x31;
    pub const WRITE_ENABLE: u32 = 0x06;
    pub const FAST_READ_QUAD: u32 = 0x6b;
    pub const FAST_READ_QUAD_OUTPUT_4B: u32 = 0x6c;
    pub const SECTOR_ERASE: u32 = 0x20;
    pub const READ_JEDEC_ID: u32 = 0x9f;
    pub const BLOCK_ERASE_64KB: u32 = 0xd8;
    pub const BLOCK_ERASE_64KB_4B: u32 = 0xdc;
    pub const QUAD_INPUT_PAGE_PROGRAM: u32 = 0x32;
    pub const QUAD_INPUT_PAGE_PROGRAM_4B: u32 = 0x34;
}

impl FlashDriver {
    fn flash_read_id(&self) -> [u8; 20] {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 20);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ_JEDEC_ID);
        self.wait_fpga_busy();
        let mut out = [0u8; 20];
        for i in 0..out.len() / 4 {
            let v = self.read_reg(reg::RX_FIFO);
            for (j, byte) in v.to_le_bytes().iter().enumerate() {
                out[i * 4 + j] = *byte;
            }
        }
        out
    }

    fn read_reg(&self, reg: *mut u32) -> u32 {
        unsafe { reg.read_volatile() }
    }

    fn write_reg(&self, reg: *mut u32, v: u32) {
        unsafe { reg.write_volatile(v) };
    }

    /// Wait until the FPGA is idle
    fn wait_fpga_busy(&self) {
        loop {
            let spi_sr = self.read_reg(reg::SPISR);
            if (spi_sr & 1) == 0 {
                break;
            }
            ringbuf_entry!(Trace::FpgaBusy { spi_sr });
            sleep_for(1);
        }
    }

    /// Wait until a word is available in the FPGA's RX buffer
    fn wait_fpga_rx(&self) {
        for i in 0.. {
            let spi_sr = self.read_reg(reg::SPISR);
            const RX_EMPTY_BIT: u32 = 1 << 6;
            if spi_sr & RX_EMPTY_BIT == 0 {
                break;
            }
            ringbuf_entry!(Trace::FpgaBusy { spi_sr });
            // Initial busy-loop for faster response
            if i >= 32 {
                sleep_for(1);
            }
        }
    }

    /// Clears the FPGA's internal FIFOs
    fn clear_fifos(&self) {
        self.write_reg(reg::SPICR, 0x8080);
    }

    /// Wait until the SPI flash is idle
    fn wait_flash_busy(&self, t: Trace) {
        // Wait for the busy flag to be unset
        while (self.read_flash_status() & 1) != 0 {
            ringbuf_entry!(t);
            sleep_for(1);
        }
    }

    /// Reads the STATUS1 register from flash
    fn read_flash_status(&self) -> u8 {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 1);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ_STATUS_1);
        self.wait_fpga_busy();
        self.read_reg(reg::RX_FIFO).to_le_bytes()[0]
    }

    /// Sets the write enable flag in flash
    fn flash_write_enable(&self) {
        self.write_reg(reg::DATA_BYTES, 0);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::WRITE_ENABLE);
        self.wait_fpga_busy();
    }

    /// Erases the 64KiB flash sector containing the given address
    fn flash_sector_erase(&mut self, addr: u32) {
        self.flash_write_enable();
        self.write_reg(reg::DATA_BYTES, 0);
        self.write_reg(reg::ADDR, addr);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::BLOCK_ERASE_64KB_4B);

        // Wait for the busy flag to be unset
        self.wait_flash_busy(Trace::SectorEraseBusy);
    }

    /// Reads data from the given address into a `BufWriter`
    fn flash_read(
        &mut self,
        offset: u32,
        dest: &mut dyn idol_runtime::BufWriter<'_>,
    ) -> Result<(), ()> {
        loop {
            let len = dest.remaining_size().min(PAGE_SIZE_BYTES);
            if len == 0 {
                break;
            }
            self.clear_fifos();
            self.write_reg(reg::DATA_BYTES, len as u32);
            self.write_reg(reg::ADDR, offset);
            self.write_reg(reg::DUMMY_CYCLES, 8);
            self.write_reg(reg::INSTR, instr::FAST_READ_QUAD_OUTPUT_4B);
            for i in 0..len.div_ceil(4) {
                self.wait_fpga_rx();
                let v = self.read_reg(reg::RX_FIFO);
                let v = v.to_le_bytes();
                for (j, byte) in v.iter().enumerate() {
                    let k = i * 4 + j;
                    if k < len {
                        dest.write(*byte)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes data from a `BufReader` into the flash
    fn flash_write(
        &mut self,
        addr: u32,
        data: &mut dyn idol_runtime::BufReader<'_>,
    ) -> Result<(), ()> {
        loop {
            let len = data.remaining_size().min(PAGE_SIZE_BYTES);
            if len == 0 {
                break;
            }

            self.flash_write_enable();
            self.write_reg(reg::DATA_BYTES, len as u32);
            self.write_reg(reg::ADDR, addr);
            self.write_reg(reg::DUMMY_CYCLES, 0);
            for i in 0..len.div_ceil(4) {
                let mut v = [0u8; 4];
                for (j, byte) in v.iter_mut().enumerate() {
                    let k = i * 4 + j;
                    if k < len {
                        let Some(d) = data.read() else {
                            return Err(());
                        };
                        *byte = d;
                    }
                }
                let v = u32::from_le_bytes(v);
                self.write_reg(reg::TX_FIFO, v);
            }
            self.write_reg(reg::INSTR, instr::QUAD_INPUT_PAGE_PROGRAM_4B);
            self.wait_fpga_busy();

            // Wait for the busy flag to be unset
            self.wait_flash_busy(Trace::WriteBusy);
        }
        Ok(())
    }

    /// Enable the quad enable bit in flash
    fn flash_set_quad_enable(&self) {
        let mut status = self.read_flash_status_2();
        status |= 1 << 1; // QE bit
        self.write_flash_status_2(status);
    }

    fn read_flash_status_2(&self) -> u8 {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 1);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::INSTR, instr::READ_STATUS_2);
        self.wait_fpga_busy();
        self.read_reg(reg::RX_FIFO).to_le_bytes()[0]
    }

    fn write_flash_status_2(&self, v: u8) {
        self.clear_fifos();
        self.write_reg(reg::DATA_BYTES, 1);
        self.write_reg(reg::ADDR, 0);
        self.write_reg(reg::DUMMY_CYCLES, 0);
        self.write_reg(reg::TX_FIFO, u32::from_le_bytes([v, 0, 0, 0]));
        self.write_reg(reg::INSTR, instr::WRITE_STATUS_2);
        self.wait_fpga_busy();
    }
}

/// Failure function, running an Idol response loop that always returns an error
fn fail(err: drv_hf_api::HfError) {
    let mut buffer = [0; hf::idl::INCOMING_SIZE];
    let mut server = hf::FailServer { err };
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
