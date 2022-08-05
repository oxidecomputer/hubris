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

use drv_update_api::UpdateError;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R};
use ringbuf::*;
use stm32h7::stm32h753 as device;
use userlib::*;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.2
const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xCDEF_89AB;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.3
const FLASH_OPT_KEY1: u32 = 0x0819_2A3B;
const FLASH_OPT_KEY2: u32 = 0x4C5D_6E7F;

const BANK_ADDR: u32 = 0x08100000;
const BANK_END: u32 = 0x08200000;

// Writes are indexed by flash words, BANK_ADDR is word 0,
// BANK_ADDR + FLASH_WORD_BYTES is word 1 etc.
const BANK_WORD_LIMIT: usize =
    (BANK_END - BANK_ADDR) as usize / FLASH_WORD_BYTES;

// RM0433 Rev 7 section 4.3.9
// Flash word is defined as 256 bits
const FLASH_WORD_BITS: usize = 256;

// Total length of a word in bytes (i.e. our array size)
const FLASH_WORD_BYTES: usize = FLASH_WORD_BITS / 8;

// Block is an abstract concept here. It represents the size of data the
// driver will process at a time.
const BLOCK_SIZE_BYTES: usize = FLASH_WORD_BYTES * 32;

// Must match app.toml!
const FLASH_IRQ: u32 = 1 << 0;

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
}

impl<'a> ServerImpl<'a> {
    // See RM0433 Rev 7 section 4.3.13
    fn swap_banks(&mut self) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::FinishStart);
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

        if err.dbeccerr().bit() {
            return Err(UpdateError::EccDoubleErr.into());
        }

        if err.sneccerr1().bit() {
            return Err(UpdateError::EccSingleErr.into());
        }

        if err.rdserr().bit() {
            return Err(UpdateError::SecureErr.into());
        }

        if err.rdperr().bit() {
            return Err(UpdateError::ReadProtErr.into());
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
        bytes: &[u8],
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::WriteStart);

        if word_number > BANK_WORD_LIMIT {
            panic!();
        }

        let start = BANK_ADDR + (word_number * FLASH_WORD_BYTES) as u32;

        if bytes.len() != FLASH_WORD_BYTES {
            return Err(UpdateError::BadLength.into());
        }

        if start + (bytes.len() as u32) > BANK_END {
            return Err(UpdateError::BadLength.into());
        }

        self.flash.bank2().cr.write(|w| {
            // SAFETY
            // The `psize().bits(_)` function is marked unsafe in the stm32
            // crate because it allows arbitrary bit patterns. `0b11`
            // corresponds to 64-bit parallelism.
            unsafe { w.psize().bits(0b11) }.pg().set_bit()
        });

        for (i, c) in bytes.chunks_exact(4).enumerate() {
            let mut word: [u8; 4] = [0; 4];
            word.copy_from_slice(&c);

            // SAFETY
            // This code is running out of bank #1. The programming for bank #2
            // is completely separate so it will not affect running code.
            // The address is bounds checked against the start and end of
            // the bank limits.
            unsafe {
                core::ptr::write_volatile(
                    (start + (i * 4) as u32) as *mut u32,
                    u32::from_le_bytes(word),
                );
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

    fn bank_erase(&mut self) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::EraseStart);

        // Enable relevant interrupts for completion (or failure) of erasing
        // bank2.
        sys_irq_control(FLASH_IRQ, true);
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
            sys_recv_closed(&mut [], FLASH_IRQ, TaskId::KERNEL).unwrap_lite();
            if self.flash.bank2().sr.read().eop().bit() {
                break;
            } else {
                sys_irq_control(FLASH_IRQ, true);
            }
        }

        let b = self.bank2_status();
        ringbuf_entry!(Trace::EraseEnd);
        b
    }
}

impl idl::InOrderUpdateImpl for ServerImpl<'_> {
    fn prep_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::InProgress | UpdateState::Finished => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            _ => (),
        }

        self.unlock();
        self.bank_erase()?;
        self.state = UpdateState::InProgress;
        Ok(())
    }

    fn write_one_block(
        &mut self,
        _: &RecvMessage,
        block_num: usize,
        block: LenLimit<Leased<R, [u8]>, BLOCK_SIZE_BYTES>,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate | UpdateState::Finished => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            _ => (),
        }

        let len = block.len();
        let mut flash_page: [u8; BLOCK_SIZE_BYTES] = [0; BLOCK_SIZE_BYTES];

        block
            .read_range(0..len as usize, &mut flash_page)
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        // If there is a write less than the block size zero out the trailing
        // bytes
        if len < BLOCK_SIZE_BYTES {
            flash_page[len..].fill(0);
        }

        ringbuf_entry!(Trace::WriteBlock(block_num as usize));
        for (i, c) in flash_page.chunks(FLASH_WORD_BYTES).enumerate() {
            const FLASH_WORDS_PER_BLOCK: usize =
                BLOCK_SIZE_BYTES / FLASH_WORD_BYTES;

            self.write_word(block_num * FLASH_WORDS_PER_BLOCK + i, &c)?;
        }

        Ok(())
    }

    fn finish_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate | UpdateState::Finished => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            _ => (),
        }

        self.swap_banks()?;
        self.state = UpdateState::Finished;
        Ok(())
    }

    fn block_size(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<UpdateError>> {
        Ok(BLOCK_SIZE_BYTES)
    }
}

#[export_name = "main"]
fn main() -> ! {
    let flash = unsafe { &*device::FLASH::ptr() };

    let mut server = ServerImpl {
        flash,
        state: UpdateState::NoUpdate,
    };
    let mut incoming = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    use super::UpdateError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
