// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver for FMC-attached NOR flash, implementing the `hf` API
//!
//! The NOR flash chip is a W25Q01JVZEIQ, which is a 1 GBit NOR flash.  It is
//! connected to the FPGA over SPI / QSPI.
//!
//! # References
//! - [SPI NOR controller](https://github.com/oxidecomputer/quartz/tree/main/hdl/ip/vhd/spi_nor_controller/docs)

#![no_std]
#![no_main]

use ringbuf::{counted_ringbuf, ringbuf_entry};
use userlib::{hl::sleep_for, task_slot};

mod hf; // implementation of `HostFlash` API

task_slot!(LOADER, spartan7_loader);

#[derive(Debug, Clone, Copy, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    FpgaBusy,
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

/// Total flash size is 128 MiB
pub const FLASH_SIZE_BYTES: u32 = 128 * 1024 * 1024;

#[export_name = "main"]
fn main() -> ! {
    // Wait for the FPGA to be configured; the sequencer task only starts its
    // Idol loop after the FPGA has been brought up.
    let seq =
        drv_spartan7_loader_api::Spartan7Loader::from(LOADER.get_task_id());

    let drv = FlashDriver {
        drv: fmc_periph::SpiNor::new(seq.get_token()),
    };
    drv.flash_set_quad_enable();

    // Check the flash chip's ID against Table 7.3.1 in the datasheet
    let id = drv.flash_read_id();
    if id[0..3] != [0xef, 0x40, 0x21] {
        fail(drv_hf_api::HfError::BadChipId);
    }

    let mut server = hf::ServerImpl::new(drv);
    let mut buffer = [0; hf::idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

/// Absolute memory address
#[derive(Copy, Clone)]
struct FlashAddr(u32);

impl FlashAddr {
    fn new(v: u32) -> Option<Self> {
        if v < FLASH_SIZE_BYTES {
            Some(FlashAddr(v))
        } else {
            None
        }
    }
    fn get(&self) -> u32 {
        self.0
    }
}

/// Driver for a QSPI NOR flash controlled by an FPGA over FMC
struct FlashDriver {
    drv: fmc_periph::SpiNor,
}

#[allow(unused)]
mod instr {
    pub const PAGE_PROGRAM: u8 = 0x02;
    pub const READ: u8 = 0x03;
    pub const READ_STATUS_1: u8 = 0x05;
    pub const READ_STATUS_2: u8 = 0x35;
    pub const READ_STATUS_3: u8 = 0x15;
    pub const WRITE_STATUS_2: u8 = 0x31;
    pub const WRITE_ENABLE: u8 = 0x06;
    pub const FAST_READ_QUAD: u8 = 0x6b;
    pub const FAST_READ_QUAD_OUTPUT_4B: u8 = 0x6c;
    pub const SECTOR_ERASE: u8 = 0x20;
    pub const READ_JEDEC_ID: u8 = 0x9f;
    pub const BLOCK_ERASE_64KB: u8 = 0xd8;
    pub const BLOCK_ERASE_64KB_4B: u8 = 0xdc;
    pub const QUAD_INPUT_PAGE_PROGRAM: u8 = 0x32;
    pub const QUAD_INPUT_PAGE_PROGRAM_4B: u8 = 0x34;
}

impl FlashDriver {
    fn flash_read_id(&self) -> [u8; 20] {
        self.clear_fifos();
        self.drv.data_bytes.set_count(20);
        self.drv.addr.set_addr(0);
        self.drv.dummy_cycles.set_count(0);
        self.drv.instr.set_opcode(instr::READ_JEDEC_ID);
        self.wait_fpga_busy();
        let mut out = [0u8; 20];
        for i in 0..out.len() / 4 {
            let v = self.drv.rx_fifo_rdata.fifo_data();
            for (j, byte) in v.to_le_bytes().iter().enumerate() {
                out[i * 4 + j] = *byte;
            }
        }
        out
    }

    /// Wait until the FPGA is idle
    fn wait_fpga_busy(&self) {
        loop {
            if !self.drv.spisr.busy() {
                break;
            }
            ringbuf_entry!(Trace::FpgaBusy);
            sleep_for(1);
        }
    }

    /// Wait until a word is available in the FPGA's RX buffer
    fn wait_fpga_rx(&self) {
        for i in 0.. {
            if !self.drv.spisr.rx_empty() {
                break;
            }
            ringbuf_entry!(Trace::FpgaBusy);
            // Initial busy-loop for faster response
            if i >= 32 {
                sleep_for(1);
            }
        }
    }

    /// Clears the FPGA's internal FIFOs
    fn clear_fifos(&self) {
        // TODO make this a single `modify` operation?
        self.drv.spicr.modify(|r| {
            r.set_rx_fifo_reset(true);
            r.set_tx_fifo_reset(true);
        });
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
        self.drv.data_bytes.set_count(1);
        self.drv.addr.set_addr(0);
        self.drv.dummy_cycles.set_count(0);
        self.drv.instr.set_opcode(instr::READ_STATUS_1);
        self.wait_fpga_busy();
        self.drv.rx_fifo_rdata.fifo_data().to_le_bytes()[0]
    }

    /// Sets the write enable flag in flash
    fn flash_write_enable(&self) {
        self.drv.data_bytes.set_count(0);
        self.drv.addr.set_addr(0);
        self.drv.dummy_cycles.set_count(0);
        self.drv.instr.set_opcode(instr::WRITE_ENABLE);
        self.wait_fpga_busy();
    }

    /// Erases the 64KiB flash sector containing the given address
    fn flash_sector_erase(&mut self, addr: FlashAddr) {
        self.flash_write_enable();
        self.drv.data_bytes.set_count(0);
        self.drv.addr.set_addr(addr.0);
        self.drv.dummy_cycles.set_count(0);
        self.drv.instr.set_opcode(instr::BLOCK_ERASE_64KB_4B);
        self.wait_fpga_busy();

        // Wait for the busy flag to be unset
        self.wait_flash_busy(Trace::SectorEraseBusy);
    }

    /// Reads data from the given address into a `BufWriter`
    ///
    /// This function will only return an error if it fails to read from a
    /// provided lease; when given a slice, it is infallible.
    fn flash_read(
        &mut self,
        offset: FlashAddr,
        dest: &mut dyn idol_runtime::BufWriter<'_>,
    ) -> Result<(), ()> {
        loop {
            let len = dest.remaining_size().min(PAGE_SIZE_BYTES);
            if len == 0 {
                break;
            }
            self.clear_fifos();
            self.drv.data_bytes.set_count(len as u16);
            self.drv.addr.set_addr(offset.0);
            self.drv.dummy_cycles.set_count(8);
            self.drv.instr.set_opcode(instr::FAST_READ_QUAD_OUTPUT_4B);
            for i in 0..len.div_ceil(4) {
                self.wait_fpga_rx();
                let v = self.drv.rx_fifo_rdata.fifo_data().to_le_bytes();
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
    ///
    /// This function will only return an error if it fails to write to a
    /// provided lease; when given a slice, it is infallible.
    fn flash_write(
        &mut self,
        addr: FlashAddr,
        data: &mut dyn idol_runtime::BufReader<'_>,
    ) -> Result<(), ()> {
        loop {
            let len = data.remaining_size().min(PAGE_SIZE_BYTES);
            if len == 0 {
                break;
            }
            self.flash_write_enable();
            self.drv.data_bytes.set_count(len as u16);
            self.drv.addr.set_addr(addr.0);
            self.drv.dummy_cycles.set_count(0);
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
                self.drv.tx_fifo_wdata.set_fifo_data(v);
            }
            self.drv.instr.set_opcode(instr::QUAD_INPUT_PAGE_PROGRAM_4B);
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
        self.drv.data_bytes.set_count(1);
        self.drv.addr.set_addr(0);
        self.drv.dummy_cycles.set_count(0);
        self.drv.instr.set_opcode(instr::READ_STATUS_2);
        self.wait_fpga_busy();
        self.drv.rx_fifo_rdata.fifo_data().to_le_bytes()[0]
    }

    fn write_flash_status_2(&self, v: u8) {
        self.clear_fifos();
        self.drv.data_bytes.set_count(1);
        self.drv.addr.set_addr(0);
        self.drv.dummy_cycles.set_count(0);
        self.drv
            .tx_fifo_wdata
            .set_fifo_data(u32::from_le_bytes([v, 0, 0, 0]));
        self.drv.instr.set_opcode(instr::WRITE_STATUS_2);
        self.wait_fpga_busy();
    }

    fn get_flash_mux_state(&self) -> drv_hf_api::HfMuxState {
        if self.drv.spicr.sp5_owns_flash() {
            drv_hf_api::HfMuxState::HostCPU
        } else {
            drv_hf_api::HfMuxState::SP
        }
    }

    /// Returns an error if the flash mux state is not `HfMuxState::SP`
    fn check_flash_mux_state(&self) -> Result<(), drv_hf_api::HfError> {
        match self.get_flash_mux_state() {
            drv_hf_api::HfMuxState::SP => Ok(()),
            drv_hf_api::HfMuxState::HostCPU => {
                Err(drv_hf_api::HfError::NotMuxedToSP)
            }
        }
    }

    fn set_flash_mux_state(&self, ms: drv_hf_api::HfMuxState) {
        self.drv.spicr.modify(|v| match ms {
            drv_hf_api::HfMuxState::SP => v.set_sp5_owns_flash(false),
            drv_hf_api::HfMuxState::HostCPU => v.set_sp5_owns_flash(true),
        });
    }

    fn set_espi_addr_offset(&self, v: FlashAddr) {
        // The SP5 does all of its reads from a particular base address (found
        // by sniffing the SPI bus), so we have to subtract that out when
        // calculating the flash offset used by the FPGA
        const SP5_BASE: u32 = 0x3000000;
        self.drv
            .sp5_flash_offset
            .set_offset(v.0.wrapping_sub(SP5_BASE));
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

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_periph.rs"));
}
