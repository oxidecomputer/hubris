// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::hint::assert_unchecked;

use drv_auxflash_api::{
    AuxFlashBlob, AuxFlashChecksum, AuxFlashError, AuxFlashId,
    TlvcReadAuxFlash, PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES, SLOT_COUNT,
    SLOT_SIZE,
};
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::{hl, task_slot, RecvMessage};

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_qspi::{Qspi, QspiError, ReadSetting};
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
        let addr = u32::try_from(offset)
            .ok()
            .and_then(|offset| self.base.checked_add(offset))
            .ok_or(TlvcReadError::User(AuxFlashError::AddressOverflow))?;
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
    let qspi = Qspi::new(
        reg,
        notifications::QSPI_IRQ_MASK,
        if cfg!(feature = "fast-qspi") {
            ReadSetting::Quad
        } else {
            ReadSetting::Single
        },
    );

    let clock = if cfg!(feature = "fast-qspi") {
        3 // 200MHz kernel / 3 = 66MHz clock
    } else {
        5 // 200MHz kernel / 5 = 40MHz clock
    };
    const MEMORY_SIZE: usize = SLOT_COUNT as usize * SLOT_SIZE;
    let memory_size_log2 = const {
        assert!(MEMORY_SIZE.is_power_of_two());
        let memory_size_log2 = MEMORY_SIZE.trailing_zeros();
        if memory_size_log2 > u8::MAX as u32 {
            panic!();
        }
        memory_size_log2 as u8
    };
    qspi.configure(clock, memory_size_log2);

    // This driver is compatible with Sidecar, Cosmo, and Grapefruit; Gimlet
    // uses its QSPI peripheral for host flash, and would have to be handled
    // differently.
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
    // Board      | Part number       | Designator | QSPI | Used
    // -----------|-------------------|------------|------|------
    // Gimlet     | W25N01GVZEIG      | U557       | No   | No
    // Sidecar    | W25Q256JVEIQ      | U63        | Yes  | Yes
    // Cosmo      | W25Q256JVEIQ      | U21        | Yes  | Yes
    // Grapefruit | W25Q256JVEIQ      | U10        | Yes  | Yes
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
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }
        // SAFETY: active_slot and spare_slot are valid slot numbers. The
        // active slot is chosen using scan_for_active_slot and never
        // reassigned, while spare_slot is its dual. Slots are always created
        // in pairs as per above check, so flipping the 0th bit keeps slot
        // validity.
        unsafe {
            assert_unchecked(active_slot < SLOT_COUNT);
            assert_unchecked(spare_slot < SLOT_COUNT);
        }
        // SAFETY: spare_slot is a valid slot number, as slots are created in
        // pairs.
        let spare_checksum = self.read_slot_checksum(spare_slot);
        if spare_checksum.map(|c| c.0) == Ok(AUXI_CHECKSUM) {
            return Ok(());
        }

        let active_slot_base = active_slot * SLOT_SIZE as u32;
        // Find the length of data by finding the final TLV-C slot
        let handle = SlotReader {
            qspi: &self.qspi,
            base: active_slot_base,
        };
        let mut reader = TlvcReader::begin(handle)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;
        while let Ok(Some(..)) = reader.next() {
            // Nothing to do here
        }
        // SAFETY: this cannot wrap, as our reader's max size is SLOT_SIZE.
        let data_size =
            unsafe { SLOT_SIZE.unchecked_sub(reader.remaining() as usize) };

        let mut buf = [0u8; PAGE_SIZE_BYTES];
        let mut read_addr = active_slot_base as usize;
        // Note: this cannot overflow as spare slot was checked above.
        let mut write_addr = spare_slot as usize * SLOT_SIZE;
        // SAFETY: this cannot overflow as data_size is less or equal to
        // SLOT_SIZE and active_slot is valid for reads up to SLOT_SIZE.
        let read_end = unsafe { read_addr.unchecked_add(data_size) };
        while read_addr < read_end {
            let amount = (read_end - read_addr).min(buf.len());

            // Read from the active slot
            self.qspi
                .read_memory(read_addr as u32, &mut buf[..amount])
                .map_err(qspi_to_auxflash)?;

            // If we're at the start of a sector, erase it before we start
            // writing the copy.
            if write_addr.is_multiple_of(SECTOR_SIZE_BYTES) {
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

            // SAFETY: these cannot overflow as amount is at most
            // 'read_end - read_addr'; we can at most reach read_end here.
            unsafe {
                read_addr = read_addr.unchecked_add(amount);
                write_addr = write_addr.unchecked_add(amount);
            }
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
        let mfr_id = idbuf[0];
        let memory_type = idbuf[1];
        let capacity = idbuf[2];

        let unique_id = self
            .qspi
            .read_winbond_unique_id()
            .map_err(qspi_to_auxflash)?;
        Ok(AuxFlashId {
            mfr_id,
            memory_type,
            capacity,
            unique_id,
        })
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
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }
        let mem_start: usize = slot as usize * SLOT_SIZE;
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
            self.poll_for_write_complete(Some(1))?;
            if let Some(next) = addr.checked_add(SECTOR_SIZE_BYTES) {
                addr = next;
            } else {
                // Technically it's possible that SECTOR_SIZE_BYTES > SLOT_SIZE,
                // in which case this overflow. In that case we overflow mem_end
                // as well and have arrived at the end of the loop.
                break;
            }
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
        } else if offset >= SLOT_SIZE as u32 {
            return Err(AuxFlashError::AddressOverflow.into());
        }
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }
        // Note: these cannot overflow as per above checks.
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
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        } else if Some(slot) == self.active_slot {
            return Err(AuxFlashError::SlotActive.into());
        } else if !(offset as usize).is_multiple_of(PAGE_SIZE_BYTES) {
            return Err(AuxFlashError::UnalignedAddress.into());
        } else if (offset as usize)
            .checked_add(data.len())
            .is_none_or(|len| len > SLOT_SIZE)
        {
            return Err(AuxFlashError::AddressOverflow.into());
        }
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }
        // Note: these cannot overflow as per above checks.
        let mem_start = slot as usize * SLOT_SIZE + offset as usize;
        let data_len = data.len();
        let mem_end = mem_start + data_len;
        if mem_end > u32::MAX as usize {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        // The flash chip has a limited write buffer!
        let mut buf = [0u8; PAGE_SIZE_BYTES];
        let mut mem_offset = 0usize;
        while mem_offset < data_len {
            let amount = (data_len - mem_offset).min(buf.len());
            // SAFETY: amount takes us to data_len in at most buf sized
            // chunks. The next offset is always at most data_len.
            let next_mem_offset = unsafe { mem_offset.unchecked_add(amount) };
            let buf = &mut buf[..amount];
            data.read_range(mem_offset..next_mem_offset, buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            self.set_and_check_write_enable()?;
            // SAFETY: mem_offset goes from 0..data_len, while mem_start +
            // data_len was checked to not overflow. we're thus always within
            // bounds.
            let page_addr: usize =
                unsafe { mem_start.unchecked_add(mem_offset) };
            self.qspi
                .page_program(page_addr as u32, buf)
                .map_err(qspi_to_auxflash)?;
            self.poll_for_write_complete(None)?;
            mem_offset = next_mem_offset;
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
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        } else if (offset as usize)
            .checked_add(dest.len())
            .is_none_or(|len| len > SLOT_SIZE)
        {
            // Adding offset to destination length would overflow usize or slot
            // size.
            return Err(AuxFlashError::AddressOverflow.into());
        }
        const {
            assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some());
            assert!(SLOT_COUNT > 0 && SLOT_COUNT.is_multiple_of(2));
        }

        let mut addr = slot as usize * SLOT_SIZE + offset as usize;
        let end = addr + dest.len();

        let mut write = 0usize;
        let mut buf = [0u8; 256];
        while addr < end {
            let amount = (end - addr).min(buf.len());
            let buf = &mut buf[..amount];
            self.qspi
                .read_memory(addr as u32, buf)
                .map_err(qspi_to_auxflash)?;
            // SAFETY: amount takes us from addr to end in at most buf.len()
            // sized chunks. write consequently goes from 0..(end - addr) in
            // those chunks.
            let write_end = unsafe { write.unchecked_add(amount) };
            dest.write_range(write..write_end, buf)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            write = write_end;
            // SAFETY: see above; addr + amount <= end.
            addr = unsafe { addr.unchecked_add(amount) };
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
        // SAFETY: active_slot is a valid slot number chosen using
        // scan_for_active_slot and never reassigned.
        unsafe { assert_unchecked(active_slot < SLOT_COUNT) };
        const { assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some()) };
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

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
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
    const { assert!(SLOT_COUNT.checked_mul(SLOT_SIZE as u32).is_some()) };
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
