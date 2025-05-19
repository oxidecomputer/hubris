// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_auxflash_api::{
    AuxFlashBlob, AuxFlashChecksum, AuxFlashError, AuxFlashId,
    TlvcReadAuxFlash, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES, SLOT_COUNT,
    SLOT_SIZE,
};
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::{hl, task_slot, RecvMessage, UnwrapLite};

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_qspi::{Qspi, QspiError};
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);

////////////////////////////////////////////////////////////////////////////////

/// Simple handle which holds a `&Qspi` and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
struct SlotReader<'a> {
    qspi: &'a Qspi,
    base: u32,
}

impl<'a> TlvcRead for SlotReader<'a> {
    type Error = AuxFlashError;

    fn extent(&self) -> Result<u64, TlvcReadError<Self::Error>> {
        // Hard-coded slot size, on a per-board basis
        Ok(SLOT_SIZE as u64)
    }
    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError<Self::Error>> {
        let addr: u32 = self.base + u32::try_from(offset).unwrap_lite();
        self.qspi
            .read_memory(addr, dest)
            .map_err(|x| TlvcReadError::User(qspi_to_auxflash(x)))?;
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////

// There isn't a great crate to do `From` implementation so do this manually
fn qspi_to_auxflash(val: QspiError) -> AuxFlashError {
    match val {
        QspiError::Timeout => AuxFlashError::QspiTimeout,
        QspiError::TransferError => AuxFlashError::QspiTransferError,
    }
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, notifications::QSPI_IRQ_MASK);

    let clock = 5; // 200MHz kernel / 5 = 40MHz clock
    const MEMORY_SIZE: usize = SLOT_COUNT as usize * SLOT_SIZE;
    assert!(MEMORY_SIZE.is_power_of_two());
    let memory_size_log2 = MEMORY_SIZE.trailing_zeros().try_into().unwrap();
    qspi.configure(clock, memory_size_log2);

    // Sidecar-only for now!
    //
    // This is mostly copied from `gimlet-hf-server`, with a few pin adjustments
    //
    // SP_QSPI_RESET_L     PF5     GPIO
    // SP_QSPI_CLK         PF10    QUADSPI_CLK
    // SP_QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
    // SP_QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
    // SP_QSPI_CS_L        PG6     QUADSPI_BK1_NCS (or GPIO?)
    // SP_QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
    // SP_QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(15);

    // TODO: check the ID and make sure it's what we expect
    //
    // Gimlet is  MT25QU256ABA8E12
    // Sidecar is S25FL128SAGMFIR01
    let mut buffer = [0; idl::INCOMING_SIZE];
    let active_slot = scan_for_active_slot(&qspi);
    let mut server = ServerImpl { qspi, active_slot };

    let _ = server.ensure_redundancy();

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    qspi: Qspi,
    active_slot: Option<u32>,
}

impl ServerImpl {
    /// Polls for the "Write Complete" flag.
    ///
    /// Sleep times are in ticks (typically milliseconds) and are somewhat
    /// experimentally determined, see hubris#753 for details.
    fn poll_for_write_complete(
        &self,
        sleep: Option<u64>,
    ) -> Result<(), AuxFlashError> {
        loop {
            let status = self.qspi.read_status().map_err(qspi_to_auxflash)?;
            if status & 1 == 0 {
                // ooh we're done
                break;
            }
            if let Some(sleep) = sleep {
                hl::sleep_for(sleep);
            }
        }
        Ok(())
    }

    fn set_and_check_write_enable(&self) -> Result<(), AuxFlashError> {
        self.qspi.write_enable().map_err(qspi_to_auxflash)?;
        let status = self.qspi.read_status().map_err(qspi_to_auxflash)?;

        if status & 0b10 == 0 {
            // oh oh
            return Err(AuxFlashError::WriteEnableFailed);
        }
        Ok(())
    }

    fn read_slot_checksum(
        &self,
        slot: u32,
    ) -> Result<AuxFlashChecksum, AuxFlashError> {
        read_and_check_slot_checksum(&self.qspi, slot)
    }

    /// Checks that the matched slot in this even/odd pair also has valid data.
    ///
    /// If not, writes the auxiliary data to the spare slot.
    fn ensure_redundancy(&mut self) -> Result<(), AuxFlashError> {
        let active_slot =
            self.active_slot.ok_or(AuxFlashError::NoActiveSlot)?;

        let spare_slot = active_slot ^ 1;
        let spare_checksum = self.read_slot_checksum(spare_slot);
        if spare_checksum.map(|c| c.0) == Ok(AUXI_CHECKSUM) {
            return Ok(());
        }

        // Find the length of data by finding the final TLV-C slot
        let handle = SlotReader {
            qspi: &self.qspi,
            base: active_slot * SLOT_SIZE as u32,
        };
        let mut reader = TlvcReader::begin(handle)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;
        while let Ok(Some(..)) = reader.next() {
            // Nothing to do here
        }
        let data_size = SLOT_SIZE - reader.remaining() as usize;

        let mut buf = [0u8; PAGE_SIZE_BYTES];
        let mut read_addr = active_slot as usize * SLOT_SIZE;
        let mut write_addr = spare_slot as usize * SLOT_SIZE;
        let read_end = read_addr + data_size;
        while read_addr < read_end {
            let amount = (read_end - read_addr).min(buf.len());

            // Read from the active slot
            self.qspi
                .read_memory(read_addr as u32, &mut buf[..amount])
                .map_err(qspi_to_auxflash)?;

            // If we're at the start of a sector, erase it before we start
            // writing the copy.
            if write_addr % SECTOR_SIZE_BYTES == 0 {
                self.set_and_check_write_enable()?;
                self.qspi
                    .sector_erase(write_addr as u32)
                    .map_err(qspi_to_auxflash)?;
                self.poll_for_write_complete(Some(1))?;
            }

            // Write back to the redundant slot
            self.set_and_check_write_enable()?;
            self.qspi
                .page_program(write_addr as u32, &buf[..amount])
                .map_err(qspi_to_auxflash)?;
            self.poll_for_write_complete(None)?;

            read_addr += amount;
            write_addr += amount;
        }

        // Confirm that the spare write worked
        let spare_checksum = self.read_slot_checksum(spare_slot)?;
        if spare_checksum.0 == AUXI_CHECKSUM {
            Ok(())
        } else {
            Err(AuxFlashError::ChckMismatch)
        }
    }
}

impl idl::InOrderAuxFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<AuxFlashId, RequestError<AuxFlashError>> {
        let mut idbuf = [0; 20];
        self.qspi.read_id(&mut idbuf).map_err(qspi_to_auxflash)?;
        Ok(AuxFlashId(idbuf))
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<AuxFlashError>> {
        Ok(self.qspi.read_status().map_err(qspi_to_auxflash)?)
    }

    fn slot_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<AuxFlashError>> {
        Ok(SLOT_COUNT)
    }

    fn slot_size(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<AuxFlashError>> {
        Ok(SLOT_SIZE as u32)
    }

    fn read_slot_chck(
        &mut self,
        _: &RecvMessage,
        slot: u32,
    ) -> Result<AuxFlashChecksum, RequestError<AuxFlashError>> {
        Ok(self.read_slot_checksum(slot)?)
    }

    fn erase_slot(
        &mut self,
        _: &RecvMessage,
        slot: u32,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        }
        let mem_start = slot as usize * SLOT_SIZE;
        let mem_end = mem_start + SLOT_SIZE;
        if mem_end > u32::MAX as usize {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        let mut addr = mem_start;
        while addr < mem_end {
            self.set_and_check_write_enable()?;
            self.qspi
                .sector_erase(addr as u32)
                .map_err(qspi_to_auxflash)?;
            addr += SECTOR_SIZE_BYTES;
            self.poll_for_write_complete(Some(1))?;
        }
        Ok(())
    }

    fn slot_sector_erase(
        &mut self,
        _: &RecvMessage,
        slot: u32,
        offset: u32,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        }
        if offset >= SLOT_SIZE as u32 {
            return Err(AuxFlashError::AddressOverflow.into());
        }
        let addr = slot as usize * SLOT_SIZE + offset as usize;
        if addr > u32::MAX as usize {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        self.set_and_check_write_enable()?;
        self.qspi
            .sector_erase(addr as u32)
            .map_err(qspi_to_auxflash)?;
        self.poll_for_write_complete(Some(1))?;
        Ok(())
    }

    fn write_slot_with_offset(
        &mut self,
        _: &RecvMessage,
        slot: u32,
        offset: u32,
        data: Leased<R, [u8]>,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if Some(slot) == self.active_slot {
            return Err(AuxFlashError::SlotActive.into());
        }
        if offset as usize % PAGE_SIZE_BYTES != 0 {
            return Err(AuxFlashError::UnalignedAddress.into());
        } else if offset as usize + data.len() > SLOT_SIZE {
            return Err(AuxFlashError::AddressOverflow.into());
        }
        let mem_start = (slot as usize * SLOT_SIZE) + offset as usize;
        let mem_end = mem_start + data.len();
        if mem_end > u32::MAX as usize {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        // The flash chip has a limited write buffer!
        let mut addr = mem_start;
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        let mut read = 0;
        while addr < mem_end {
            let amount = (mem_end - addr).min(buf.len());
            data.read_range(read..(read + amount), &mut buf[..amount])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            self.set_and_check_write_enable()?;
            self.qspi
                .page_program(addr as u32, &buf[..amount])
                .map_err(qspi_to_auxflash)?;
            self.poll_for_write_complete(None)?;
            addr += amount;
            read += amount;
        }
        Ok(())
    }

    fn read_slot_with_offset(
        &mut self,
        _: &RecvMessage,
        slot: u32,
        offset: u32,
        dest: Leased<W, [u8]>,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if offset as usize + dest.len() > SLOT_SIZE {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        let mut addr = (slot as usize * SLOT_SIZE) + offset as usize;
        let end = addr + dest.len();

        let mut write = 0;
        let mut buf = [0u8; 256];
        while addr < end {
            let amount = (end - addr).min(buf.len());
            self.qspi
                .read_memory(addr as u32, &mut buf[..amount])
                .map_err(qspi_to_auxflash)?;
            dest.write_range(write..(write + amount), &buf[..amount])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            write += amount;
            addr += amount;
        }
        Ok(())
    }

    fn scan_and_get_active_slot(
        &mut self,
        msg: &RecvMessage,
    ) -> Result<u32, RequestError<AuxFlashError>> {
        // We no longer actually "scan"; this function is now misleadingly named
        // but marked as deprecated in the idl file. We keep it around for
        // compatibility with old humility.
        self.get_active_slot(msg)
    }

    fn get_active_slot(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<AuxFlashError>> {
        self.active_slot
            .ok_or_else(|| AuxFlashError::NoActiveSlot.into())
    }

    fn ensure_redundancy(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<AuxFlashError>> {
        ServerImpl::ensure_redundancy(self).map_err(Into::into)
    }

    fn get_blob_by_tag(
        &mut self,
        _: &RecvMessage,
        tag: [u8; 4],
    ) -> Result<AuxFlashBlob, RequestError<AuxFlashError>> {
        let active_slot = self
            .active_slot
            .ok_or_else(|| RequestError::from(AuxFlashError::NoActiveSlot))?;
        let handle = SlotReader {
            qspi: &self.qspi,
            base: active_slot * SLOT_SIZE as u32,
        };
        handle
            .get_blob_by_tag(active_slot, tag)
            .map_err(RequestError::from)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

fn scan_for_active_slot(qspi: &Qspi) -> Option<u32> {
    for i in 0..SLOT_COUNT {
        let handle = SlotReader {
            qspi,
            base: i * SLOT_SIZE as u32,
        };

        let Ok(chck) = handle.read_stored_checksum() else {
            // Just skip to the next slot if it's empty or invalid.
            continue;
        };

        if chck.0 != AUXI_CHECKSUM {
            // If it's not the chunk we're interested in, don't bother hashing
            // it.
            continue;
        }

        let Ok(actual) = handle.calculate_checksum() else {
            // TODO: this ignores I/O errors, but, this is how the code has
            // always been structured...
            continue;
        };

        if chck == actual {
            return Some(i);
        }
    }
    None
}

fn read_and_check_slot_checksum(
    qspi: &Qspi,
    slot: u32,
) -> Result<AuxFlashChecksum, AuxFlashError> {
    if slot >= SLOT_COUNT {
        return Err(AuxFlashError::InvalidSlot);
    }
    let handle = SlotReader {
        qspi,
        base: slot * SLOT_SIZE as u32,
    };
    let claimed = handle.read_stored_checksum()?;
    let actual = handle.calculate_checksum()?;
    if claimed == actual {
        Ok(actual)
    } else {
        Err(AuxFlashError::ChckMismatch)
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::AuxFlashError;
    use drv_auxflash_api::{AuxFlashBlob, AuxFlashChecksum, AuxFlashId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

include!(concat!(env!("OUT_DIR"), "/checksum.rs"));
