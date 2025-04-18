// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Functions for writing to flash for updates
//
// This driver is intended to carry as little state as possible. Most of the
// heavy work and decision making should be handled in other tasks.
#![no_std]
#![no_main]

use core::convert::Infallible;
use core::ops::Range;
use drv_caboose::{CabooseError, CabooseReader};
use drv_stm32h7_update_api::{
    ImageVersion, SlotId, BLOCK_SIZE_BYTES, FLASH_WORDS_PER_BLOCK,
    FLASH_WORD_BYTES,
};
use drv_update_api::UpdateError;
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R,
};
use ringbuf::*;
use stm32h7::stm32h753 as device;
use userlib::*;
use zerocopy::AsBytes;

// Internally we deal with flash blocks in groups of u32 words.
const FLASH_WORD_WORDS: usize = FLASH_WORD_BYTES / 4;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.2
const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xCDEF_89AB;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.3
const FLASH_OPT_KEY1: u32 = 0x0819_2A3B;
const FLASH_OPT_KEY2: u32 = 0x4C5D_6E7F;

extern "C" {
    // Symbols injected by the linker.
    //
    // This requires adding `extern-regions = ["bank2"]` to the task config
    pub static mut __REGION_BANK2_BASE: [u32; 0];
    pub static mut __REGION_BANK2_END: [u32; 0];
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    EraseStart,
    EraseEnd,
    WriteStart,
    WriteEnd,
    FinishStart,
    FinishEnd,
    WriteBlock(usize),
    None,
}

enum UpdateState {
    NoUpdate,
    InProgress,
    Finished,
}

ringbuf!(Trace, 64, Trace::None);

struct ServerImpl<'a> {
    flash: &'a device::flash::RegisterBlock,
    state: UpdateState,
    pending: SlotId,
}

impl<'a> ServerImpl<'a> {
    // See RM0433 Rev 7 section 4.3.13
    fn swap_banks(&mut self) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::FinishStart);
        self.unlock();
        if self.flash.optsr_cur().read().swap_bank_opt().bit() {
            self.flash
                .optsr_prg()
                .modify(|_, w| w.swap_bank_opt().clear_bit());
        } else {
            self.flash
                .optsr_prg()
                .modify(|_, w| w.swap_bank_opt().set_bit());
        }

        self.flash.optcr().modify(|_, w| w.optstart().set_bit());

        loop {
            if !self.flash.optsr_cur().read().opt_busy().bit() {
                break;
            }
        }

        self.pending = match self.pending {
            SlotId::Active => SlotId::Inactive,
            SlotId::Inactive => SlotId::Active,
        };
        ringbuf_entry!(Trace::FinishEnd);
        Ok(())
    }

    fn poll_flash_done(&mut self) -> Result<(), RequestError<UpdateError>> {
        // This method should implement step 5 of the Single Write Sequence from
        // RM0433 Rev 7 section 4.3.9, which states
        //
        // > Check that QW1 (respectively QW2) has been raised and wait until it
        // > is reset to 0.
        //
        // However, checking that QW2 has been raised is inherently racy: it's
        // possible it was raised and lowered before we get to this method. We
        // have observed this race in practice, so we omit the check that QW2
        // has been raised and only wait until QW2 is reset to 0.
        loop {
            if !self.flash.bank2().sr.read().qw().bit() {
                break;
            }
        }

        self.bank2_status()
    }

    fn bank2_status(&self) -> Result<(), RequestError<UpdateError>> {
        let err = self.flash.bank2().sr.read();

        // We purposely omit checking `rdserr` and `rdperr` as
        // speculative access may trigger those bits

        if err.dbeccerr().bit() {
            return Err(UpdateError::EccDoubleErr.into());
        }

        if err.sneccerr1().bit() {
            return Err(UpdateError::EccSingleErr.into());
        }

        if err.operr().bit() {
            return Err(UpdateError::WriteEraseErr.into());
        }

        if err.incerr().bit() {
            return Err(UpdateError::InconsistencyErr.into());
        }

        if err.strberr().bit() {
            return Err(UpdateError::StrobeErr.into());
        }

        if err.pgserr().bit() {
            return Err(UpdateError::ProgSeqErr.into());
        }

        if err.wrperr().bit() {
            return Err(UpdateError::WriteProtErr.into());
        }

        Ok(())
    }

    // RM0433 Rev 7 section 4.3.9
    // Following Single write sequence
    fn write_word(
        &mut self,
        word_number: usize,
        words: &[u32; FLASH_WORD_WORDS],
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::WriteStart);
        self.clear_errors();

        // These variables are _philosophically_ constants, but since they're
        // generated by taking the address of a linker-generated symbol, we
        // can't define them as `const` values.
        //
        // SAFETY: these are symbols populated by the linker.
        let bank_addr = unsafe { __REGION_BANK2_BASE.as_ptr() } as usize;
        let bank_end = unsafe { __REGION_BANK2_END.as_ptr() } as usize;
        let bank_word_limit = (bank_end - bank_addr) / FLASH_WORD_BYTES;

        if word_number > bank_word_limit {
            panic!();
        }

        let start = bank_addr + (word_number * FLASH_WORD_BYTES);

        if start + FLASH_WORD_BYTES > bank_end {
            return Err(UpdateError::BadLength.into());
        }

        let addresses = (start..start + FLASH_WORD_BYTES).step_by(4);

        self.flash.bank2().cr.write(|w| {
            // SAFETY
            // The `psize().bits(_)` function is marked unsafe in the stm32
            // crate because it allows arbitrary bit patterns. `0b11`
            // corresponds to 64-bit internal parallelism during the write cycle
            // (not to be confused with the actual write accesses below, which
            // are 32-bit).
            unsafe { w.psize().bits(0b11) }.pg().set_bit()
        });

        for (addr, &word) in addresses.zip(words) {
            // SAFETY
            // This code is running out of bank #1. The programming for bank #2
            // is completely separate so it will not affect running code.
            // The address is bounds checked against the start and end of
            // the bank limits.
            unsafe {
                core::ptr::write_volatile(addr as *mut u32, word);
            }
        }

        let b = self.poll_flash_done();
        ringbuf_entry!(Trace::WriteEnd);
        b
    }

    // All sequences can be found in RM0433 Rev 7
    fn unlock(&mut self) {
        if !self.flash.bank2().cr.read().lock().bit() {
            return;
        }

        self.flash
            .bank2()
            .keyr
            .write(|w| unsafe { w.keyr().bits(FLASH_KEY1) });
        self.flash
            .bank2()
            .keyr
            .write(|w| unsafe { w.keyr().bits(FLASH_KEY2) });

        self.flash
            .optkeyr()
            .write(|w| unsafe { w.optkeyr().bits(FLASH_OPT_KEY1) });
        self.flash
            .optkeyr()
            .write(|w| unsafe { w.optkeyr().bits(FLASH_OPT_KEY2) });
    }

    fn clear_errors(&mut self) {
        // https://github.com/zephyrproject-rtos/zephyr/issues/60449
        // https://community.st.com/t5/stm32-mcus-products/spurious-rdperr-and-rdserr-when-all-protection-and-security/td-p/279852
        // There are issue with the CPU speculating into unknown
        // areas. One workaround is to explicitly mark the reigon as NX
        // via MPU but that's expensive/costs an MPU region
        // Another workaround is to just clear errors we don't
        // expect to see
        self.flash
            .bank2()
            .ccr
            .modify(|_, w| w.clr_rdperr().set_bit().clr_rdserr().set_bit());
    }

    fn bank_erase(&mut self) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::EraseStart);

        self.clear_errors();
        // Enable relevant interrupts for completion (or failure) of erasing
        // bank2.
        sys_irq_control(notifications::FLASH_IRQ_MASK, true);
        self.flash.bank2().cr.modify(|_, w| {
            w.eopie()
                .set_bit()
                .wrperrie()
                .set_bit()
                .pgserrie()
                .set_bit()
                .strberrie()
                .set_bit()
                .incerrie()
                .set_bit()
                .operrie()
                .set_bit()
        });

        self.flash
            .bank2()
            .cr
            .modify(|_, w| w.start().set_bit().ber().set_bit());

        // Wait for EOP notification via interrupt.
        loop {
            sys_recv_notification(notifications::FLASH_IRQ_MASK);
            if self.flash.bank2().sr.read().eop().bit() {
                break;
            } else {
                sys_irq_control(notifications::FLASH_IRQ_MASK, true);
            }
        }

        let b = self.bank2_status();
        ringbuf_entry!(Trace::EraseEnd);
        b
    }

    fn check_for_caboose(
        &mut self,
        image_start: u32,
        image_end: u32,
    ) -> Option<Range<u32>> {
        if image_end <= image_start {
            return None;
        }

        let caboose_size_addr = match image_end.checked_sub(4) {
            Some(e) => e,
            None => return None,
        };

        let caboose_size: u32 = unsafe {
            core::ptr::read_volatile(caboose_size_addr as *const u32)
        };

        let caboose_start = image_end.saturating_sub(caboose_size);
        if caboose_start < image_start {
            // This branch will be encountered if there's no caboose, because
            // then the nominal caboose size will be 0xFFFFFFFF, which will send
            // us out of the bank2 region.
            return None;
        } else {
            // SAFETY: we know this pointer is within the bank2 flash region,
            // since it's checked above.
            let v = unsafe {
                core::ptr::read_volatile(caboose_start as *const u32)
            };
            if v == CABOOSE_MAGIC {
                return Some(caboose_start + 4..image_end - 4);
            } else {
                return None;
            }
        };
    }

    fn find_caboose_range(&mut self) -> Result<Range<u32>, CabooseError> {
        // This code is very similar to `kipc::read_caboose_pos`, but it
        // operates on the alternate flash bank rather than on the loaded image.
        // SAFETY: populated by the linker, so this should be valid
        let image_start = unsafe { __REGION_BANK2_BASE.as_ptr() } as u32;
        let region_end = unsafe { __REGION_BANK2_END.as_ptr() } as u32;

        // If all is going according to plan, there will be a valid Hubris image
        // flashed into the other slot, delimited by `__REGION_BANK2_BASE` and
        // `__REGION_BASE2_END` (which are symbols injected by the linker).
        //
        // We'll first want to read the image header, which is at a fixed
        // location at the end of the vector table.  The length of the vector
        // table is fixed in hardware, so this should never change.
        const HEADER_OFFSET: u32 = 0x298;
        let header: ImageHeader = unsafe {
            core::ptr::read_volatile(
                (image_start + HEADER_OFFSET) as *const ImageHeader,
            )
        };
        if header.magic == 0xa {
            //return Err(CabooseError::NoImageHeader);

            // Calculate where the image header implies that the image should end
            //
            // This is a one-past-the-end value.
            let image_end = image_start + header.total_image_len;

            // Then, check that value against the BANK2 bounds.
            if image_end > region_end {
                return Err(CabooseError::MissingCaboose);
            } else {
                return self
                    .check_for_caboose(image_start, image_end)
                    .ok_or(CabooseError::MissingCaboose);
            }
        }

        // Section 4.3.10 erase size is minimum 128Kbyte. Find the first
        // fully erased sector (will read as `0xff`, see line about
        // "If the application software reads back a word that has been erased,
        // all the bits will be read at 1, without ECC error.")
        const ERASE_SECTOR_SIZE: usize = 0x2_0000;
        const SIG_SIZE: u32 = 512;
        //let mut last: u32 = 0;
        for region in (image_start..region_end).step_by(ERASE_SECTOR_SIZE) {
            let s = unsafe {
                core::slice::from_raw_parts(
                    region as *const u8,
                    ERASE_SECTOR_SIZE,
                )
            };
            if s.iter().all(|&x| x == 0xff) {
                // If the first region is all unprogrammed we don't have an image
                // and therefore no caboose
                if region == image_start {
                    return Err(CabooseError::MissingCaboose);
                }
                // We have a candidate! This is the first sector that is
                // completely unprogrammed. We're going to search backwards from
                // the end of the _previous_ sector (this sector - sector size)
                // to find the last programmed word
                let check_addr = region - (ERASE_SECTOR_SIZE as u32);
                let sub = unsafe {
                    core::slice::from_raw_parts(
                        check_addr as *const u8,
                        ERASE_SECTOR_SIZE,
                    )
                };
                // A failure here is probably a bug in our algorithm?
                let (offset, _) = sub
                    .iter()
                    .rev()
                    .enumerate()
                    .find(|&(_, x)| *x != 0xff)
                    .ok_or(CabooseError::MissingCaboose)?;
                let maybe_end =
                    check_addr + ((ERASE_SECTOR_SIZE - offset) as u32);
                if let Some(r) = self.check_for_caboose(image_start, maybe_end)
                {
                    return Ok(r);
                }
                // We might have a 512 byte signature, check for that
                if let Some(new_end) = maybe_end.checked_sub(SIG_SIZE) {
                    if let Some(r) =
                        self.check_for_caboose(image_start, new_end)
                    {
                        return Ok(r);
                    }
                }
            }
        }

        return Err(CabooseError::MissingCaboose);
    }
}

impl idl::InOrderUpdateImpl for ServerImpl<'_> {
    fn set_pending_boot_slot(
        &mut self,
        _: &RecvMessage,
        slot: SlotId,
    ) -> Result<(), RequestError<UpdateError>> {
        if slot != self.pending {
            self.swap_banks()?;
        }
        Ok(())
    }

    fn get_pending_boot_slot(
        &mut self,
        _: &RecvMessage,
    ) -> Result<SlotId, RequestError<Infallible>> {
        Ok(self.pending)
    }

    fn prep_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::InProgress => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::NoUpdate => (),
        }

        self.unlock();
        self.bank_erase()?;
        self.state = UpdateState::InProgress;
        Ok(())
    }

    fn abort_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        self.state = UpdateState::NoUpdate;
        Ok(())
    }

    fn write_one_block(
        &mut self,
        _: &RecvMessage,
        block_num: usize,
        block: LenLimit<Leased<R, [u8]>, BLOCK_SIZE_BYTES>,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        let len = block.len();
        // While our input arrives as unstructured borrowed bytes, we want to
        // ensure that we've got it aligned to 32-bits for internal reasons, and
        // we make the internal structure of the flash "page" apparent: from the
        // hardware's perspective, it is actually an array of flash words,
        // grouped (by our arbitrary choice) into units of
        // FLASH_WORDS_PER_BLOCK.
        let mut flash_page: [[u32; FLASH_WORD_WORDS]; FLASH_WORDS_PER_BLOCK] =
            [[0; FLASH_WORD_WORDS]; FLASH_WORDS_PER_BLOCK];

        {
            // Write flash_page in terms of bytes:
            let flash_bytes = flash_page.as_bytes_mut();

            block
                .read_range(0..len, flash_bytes)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            // If there is a write less than the block size fill out the
            // trailing bytes with 0xff.
            if len < BLOCK_SIZE_BYTES {
                flash_bytes[len..].fill(0xff);
            }
        }

        ringbuf_entry!(Trace::WriteBlock(block_num));
        for (i, fw) in flash_page.iter().enumerate() {
            self.write_word(block_num * FLASH_WORDS_PER_BLOCK + i, fw)?;
        }

        Ok(())
    }

    fn finish_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        self.state = UpdateState::Finished;
        Ok(())
    }

    fn block_size(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<UpdateError>> {
        Ok(BLOCK_SIZE_BYTES)
    }

    fn current_version(
        &mut self,
        _: &RecvMessage,
    ) -> Result<ImageVersion, RequestError<Infallible>> {
        Ok(ImageVersion {
            epoch: HUBRIS_BUILD_EPOCH,
            version: HUBRIS_BUILD_VERSION,
        })
    }

    fn read_caboose_value(
        &mut self,
        _: &RecvMessage,
        name: [u8; 4],
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<u32, RequestError<CabooseError>> {
        let caboose_range = self.find_caboose_range()?;
        // By construction, the last word of the caboose is its size as a `u32`

        // SAFETY: this is a slice within the bank2 flash
        let caboose = unsafe {
            core::slice::from_raw_parts(
                caboose_range.start as *const u8,
                caboose_range.len(),
            )
        };

        let reader = CabooseReader::new(caboose);

        // Get the specific chunk of caboose memory that contains the requested
        // key.  This is simply a static slice within the `caboose` slice.
        let chunk = reader.get(name)?;

        // Early exit if the caller didn't provide enough space in the lease
        if chunk.len() > data.len() {
            return Err(RequestError::Fail(ClientError::BadLease))?;
        }

        // Note that we can't copy directly from the bank2 region into the
        // leased buffer, because the kernel disallows using regions marked with
        // the DMA attribute as a source when writing.
        //
        // See the detailed comment above `can_access` in `sys/kern/src/task.rs`
        // for details!
        const BUF_SIZE: usize = 16;
        let mut buf = [0u8; BUF_SIZE];
        let mut pos = 0;
        for c in chunk.chunks(BUF_SIZE) {
            let buf = &mut buf[..c.len()];
            buf.copy_from_slice(c);
            data.write_range(pos..pos + c.len(), &buf[..c.len()])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            pos += c.len();
        }

        Ok(chunk.len() as u32)
    }
}

impl NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let flash = unsafe { &*device::FLASH::ptr() };

    // If the server restarts we need to fix our pending state
    // `FLASH_OPTCR` always has our current bank swap bit while
    // `FLASH_OPTSR_CUR` has the result after we have programmed.
    // If they are the same this means we will be booking into the
    // active slot. If they differ, we will be booting into the
    // alternate slot.
    let pending = if flash.optsr_cur().read().swap_bank_opt().bit()
        == flash.optcr().read().swap_bank().bit()
    {
        SlotId::Active
    } else {
        SlotId::Inactive
    };

    let mut server = ServerImpl {
        flash,
        state: UpdateState::NoUpdate,
        pending,
    };
    let mut incoming = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

include!(concat!(env!("OUT_DIR"), "/consts.rs"));
mod idl {
    use super::{CabooseError, ImageVersion, SlotId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
