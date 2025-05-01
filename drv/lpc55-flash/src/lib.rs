// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A raw driver for the LPC55 flash controller without using the ROM.
//!
//! This driver is written in a very generic form that doesn't assume any
//! particular execution model. You can use it to implement busy waiting, or
//! an interrupt driven driver, etc.
//!
//! See the [`Flash`] type for more details.

#![no_std]

use core::ops::RangeInclusive;

/// Number of bytes per flash word.
pub const BYTES_PER_FLASH_WORD: usize = 16;

/// Number of bytes per flash page.
pub const BYTES_PER_FLASH_PAGE: usize = 512;

/// Flash words per flash page
pub const WORDS_PER_FLASH_PAGE: usize =
    BYTES_PER_FLASH_PAGE / BYTES_PER_FLASH_WORD;

/// Flash driver handle. Wraps a pointer to the flash register set and provides
/// encapsulated operations.
///
/// # Procedure for writing to flash
///
/// 1. Use `start_erase_region` to begin an erase of your desired region. Use
///    `poll_erase_or_program_result` to discover when it's done. (Note: you can
///    use `start_blank_check` to skip this step if there's a chance the flash
///    is already erased.)
/// 2. Call `start_write_row` with each 16 byte chunk of the first page you're
///    writing, 32 times, for rows 0-31. Each time, use `poll_write_result` to
///    check that it's done.
/// 3. Call `start_program` with the index of any word in the target flash page
///    to begin the programming process. (See the next section on word numbers
///    for explanation.) Use `poll_erase_or_program_result` to learn when it's
///    done.
/// 4. Repeat steps 2 and 3 for each page in the region you want to write.
///
/// # Flash addressing and word numbers
///
/// While the flash is directly addressable by the processor at byte
/// granularity, most of the flash controller operations work in terms of a
/// _word number._ Internal flash words are 16 bytes (128 bits) in length; the
/// word number is the index of a word within the flash, starting at 0.
///
/// The flash controller thinks entirely in word numbers, _including on
/// operations that deal in pages._ For example, to erase a range of pages, you
/// give the hardware a range of word numbers, _not_ page numbers, and it erases
/// the sequence of contiguous pages that contain the start and end word
/// numbers. In the interest of not introducing additional, confusable, types of
/// addresses, this API preserves this and deals only in word numbers.
///
/// Should you need to convert a direct-addressed flash location (byte address)
/// to a word number, the procedure is:
///
/// - Divide by 16 (or right-shift by 4).
/// - Mask all but the bottom 18 bits.
///
/// Note: The `start_write_row` operation does not use _absolute_ flash word
/// numbers, but uses equivalent indices within a 512-byte internal page
/// register.
pub struct Flash<'a> {
    reg: &'a lpc55_pac::flash::RegisterBlock,
}

impl<'a> Flash<'a> {
    /// Wraps a pointer to the flash controller registers in a `Flash` driver
    /// instance.
    pub fn new(reg: &'a lpc55_pac::flash::RegisterBlock) -> Self {
        Self { reg }
    }

    /// Copies 16 bytes of data into one of the rows of the flash controller's
    /// internal page register.
    ///
    /// The page register is 512 bytes long and consists of 32 rows of 16 bytes
    /// each. Data must be accumulated into the page register before being
    /// programmed.
    ///
    /// The `row_in_page` passed to this routine specifies one of the 32 rows.
    /// This means it operates a lot like a flash word number, but in the
    /// separate "address space" of the page register.
    ///
    /// This routine only starts the operation; use `poll_write_result` to learn
    /// when it has completed. Since writes are just latching values into
    /// registers, they tend to be very fast, and it may not be useful to use
    /// interrupts to wait for completion here.
    pub fn start_write_row(&mut self, row_in_page: u32, values: &[u8; 16]) {
        self.clear_status_flags();
        self.set_single_word_number(row_in_page);
        for (register, chunk) in self.reg.dataw.iter().zip(values.chunks(4)) {
            let value = u32::from_ne_bytes(chunk.try_into().unwrap());
            register.write(|w| unsafe { w.bits(value) });
        }
        self.issue_cmd(FlashCmd::Write);
    }

    /// Checks the completion status for the values expected after a
    /// `start_write_row` operation. `true` means the write has finished.
    /// `false` means it has not.
    pub fn poll_write_result(&mut self) -> bool {
        self.read_completion_status().is_some()
    }

    /// Begins an erase of all flash words in `word_range`.
    ///
    /// The start and end values of `word_range` are _word numbers._ See the
    /// discussion of word numbers on the [`Flash`] type.
    ///
    /// This routine only starts the operation; use
    /// `poll_erase_or_program_result` to learn when it has completed and
    /// whether it succeeded.
    pub fn start_erase_range(&mut self, word_range: RangeInclusive<u32>) {
        self.clear_status_flags();
        self.set_word_range(word_range);
        self.issue_cmd(FlashCmd::EraseRange);
    }

    /// Begins programming the data loaded in the page register (by previous
    /// write operations) into the flash page containing `word_number`.
    ///
    /// See the discussion of word numbers on the [`Flash`] type.
    ///
    /// This routine only starts the operation; use
    /// `poll_erase_or_program_result` to learn when it has completed and
    /// whether it succeeded.
    pub fn start_program(&mut self, word_number: u32) {
        self.clear_status_flags();
        self.set_single_word_number(word_number);
        self.issue_cmd(FlashCmd::Program);
    }

    /// Checks the completion status for the values expected after a
    /// `start_erase_range` or `start_program` operation.
    ///
    /// `None` means the operation has not yet completed.
    ///
    /// `Some(Ok(())` means the operation completed without issue.
    ///
    /// `Some(Err(FlashTimeout))` means the operation did not complete because
    /// the internal flash state machine didn't finish within the time allotted.
    /// This is the only way these operations are able to fail according to the
    /// user manual. It's not at all clear what to _do_ in this situation since
    /// the internal state machine is essentially opaque -- but you can assume
    /// that your program or erase did not succeed.
    pub fn poll_erase_or_program_result(
        &mut self,
    ) -> Option<Result<(), FlashTimeout>> {
        let s = self.read_completion_status()?;
        // Erase and program operations only define the behavior of the FAIL
        // bit, which indicates that the flash internal state machine didn't
        // finish within a generous timeout.
        Some(if s.fail { Err(FlashTimeout) } else { Ok(()) })
    }

    /// Begins a blank-check operation on the range of flash pages containing
    /// the start and end of `word_range`, inclusive.
    ///
    /// `word_range` is given in terms of _word numbers._ See the discussion of
    /// word numbers on the [`Flash`] type.
    ///
    /// This routine only starts the operation; use `poll_blank_check_result` to
    /// learn when it has completed and whether it succeeded.
    pub fn start_blank_check(&mut self, word_range: RangeInclusive<u32>) {
        self.clear_status_flags();
        self.set_word_range(word_range);
        self.issue_cmd(FlashCmd::BlankCheck);
    }

    /// Checks the completion status for the values expected after a
    /// `start_blank_check` operation.
    ///
    /// `None` means the operation is still underway.
    ///
    /// `Some(state)` means the operation has completed and the pages were found
    /// to be in the given `state`.
    pub fn poll_blank_check_result(&mut self) -> Option<ProgramState> {
        let s = self.read_completion_status()?;
        // The result is in the FAIL bit. FAIL being set means the page is
        // _not_ blank (i.e. the blank check operation "failed" to find a
        // blank page). In this case, DATAW0 contains a word number within the
        // first non-blank page that was found.
        Some(if s.fail {
            let word_number = self.reg.dataw[0].read().bits();
            ProgramState::NotBlank { word_number }
        } else {
            ProgramState::Blank
        })
    }

    /// Begins a read operation on the flash word indexed by `word_index`. This
    /// is the "indirect" method of reading flash -- the direct method is to
    /// read from the address space where flash is mapped. This method can be
    /// useful if you control the flash peripheral and need access to random
    /// areas of flash but don't want to extend your memory map to include them.
    ///
    /// `word_index` is given in terms of _word numbers._ See the discussion of
    /// word numbers on the [`Flash`] type.
    ///
    /// This routine only starts the operation; use `poll_read_result` to learn
    /// when it has completed and whether it succeeded.
    pub fn start_read(&self, word_index: u32) {
        self.clear_status_flags();
        self.set_single_word_number(word_index);
        // Read commands use DATAW0 to select unusual read modes, like bypassing
        // ECC. Here we just want a normal read of normal flash, so the proper
        // value is zero.
        self.reg.dataw[0].write(|w| unsafe { w.bits(0) });
        self.issue_cmd(FlashCmd::ReadSingleWord);
    }

    /// Checks the completion status for the values expected after a
    /// `start_read` operation.
    ///
    /// `None` means the operation is still underway.
    ///
    /// `Some(Ok(data))` means the operation has completed successfully, and the
    /// word was found to contain `data`.
    ///
    /// `Some(Err(e))` means the operation completed in failure.
    ///
    /// On success, the word is returned as an array of `u32`s in ascending
    /// address order. To turn this into the bytes you'd read at the direct bus
    /// interface, convert each `u32` to little-endian bytes (or reinterpret it
    /// in place using `zerocopy::IntoBytes`).
    pub fn poll_read_result(&self) -> Option<Result<[u32; 4], ReadError>> {
        let s = self.read_completion_status()?;
        // The manual's definition of the status bits for READ_SINGLE_WORD is a
        // bit vague -- they clearly don't expect us to be using this. This
        // driver makes the following decisions:
        //
        // 1. An ECC error means that the read was successfully issued but found
        //    bad data. So, we treat it as superceding the other, vaguer, error
        //    bits.
        // 2. Next, the ERR bit (which indicates an illegal operation) is
        //    prioritized, to try to give feedback on that case.
        // 3. Finally, if only the FAIL bit is set, we return the vague Fail
        //    code.
        // 4. If _none_ of those bits is set, we have a success.
        if s.ecc_err {
            Some(Err(ReadError::Ecc))
        } else if s.err {
            Some(Err(ReadError::IllegalOperation))
        } else if s.fail {
            Some(Err(ReadError::Fail))
        } else {
            // Success!
            Some(Ok([
                self.reg.dataw[0].read().bits(),
                self.reg.dataw[1].read().bits(),
                self.reg.dataw[2].read().bits(),
                self.reg.dataw[3].read().bits(),
            ]))
        }
    }

    /// Turns on all interrupt sources in the flash controller (FAIL, ERR, ECC,
    /// and DONE). This will cause an interrupt to pend in the NVIC when the
    /// corresponding bit in the status register is set. This does _not_ enable
    /// the interrupt in the NVIC, so to actually receive an IRQ, you will need
    /// to also do that.
    pub fn enable_interrupt_sources(&self) {
        self.reg.int_set_enable.write(|w| {
            w.fail().set_bit();
            w.err().set_bit();
            w.ecc_err().set_bit();
            w.done().set_bit();
            w
        });
    }

    /// Turns off all interrupt sources in the flash controller (FAIL, ERR, ECC,
    /// and DONE). This does _not_ clear any event that's pended in the NVIC
    /// already, so you may still get an interrupt after doing this if you don't
    /// clear pending.
    pub fn disable_interrupt_sources(&self) {
        self.reg.int_clr_enable.write(|w| {
            w.fail().set_bit();
            w.err().set_bit();
            w.ecc_err().set_bit();
            w.done().set_bit();
            w
        });
    }

    // API below this point is internal, but is exposed in case someone needs to
    // do something weird, like attack the flash controller. The routines are
    // deliberately documented less than the "official" API above.

    pub fn issue_cmd(&self, cmd: FlashCmd) {
        self.reg.cmd.write(|w| unsafe { w.cmd().bits(cmd as u32) });
    }

    pub fn set_single_word_number(&self, word_number: u32) {
        // Technically, the write to STOPA this will incur is unnecessary.
        // However, this avoids potentially undefined behavior if
        // set_single_word_number is used before a command that actually takes a
        // range. I'm being paranoid, and the cost is low.
        self.set_word_range(word_number..=word_number);
    }

    pub fn set_word_range(&self, range: RangeInclusive<u32>) {
        self.reg
            .starta
            .write(|w| unsafe { w.starta().bits(*range.start()) });
        // The code example in the user manual treats the STOPA word number for
        // flash erase as _exclusive_ -- the code example is wrong. The
        // documentation for the commands that take ranges correctly describes
        // it as being inclusive. Otherwise how would you erase the last sector?
        self.reg
            .stopa
            .write(|w| unsafe { w.stopa().bits(*range.end()) });
    }

    pub fn read_completion_status(&self) -> Option<Status> {
        let s = self.reg.int_status.read();
        if s.done().bit() {
            Some(Status {
                ecc_err: s.ecc_err().bit(),
                err: s.err().bit(),
                fail: s.fail().bit(),
            })
        } else {
            None
        }
    }

    pub fn clear_status_flags(&self) {
        self.reg.int_clr_status.write(|w| {
            w.done().set_bit();
            w.ecc_err().set_bit();
            w.err().set_bit();
            w.fail().set_bit();
            w
        });
    }

    pub fn write_page(
        &mut self,
        addr: u32,
        flash_page: &[u8; BYTES_PER_FLASH_PAGE],
        wait: fn() -> (),
    ) -> Result<(), FlashTimeout> {
        let start_word = addr / BYTES_PER_FLASH_WORD as u32;
        self.start_erase_range(
            start_word..=start_word + ((WORDS_PER_FLASH_PAGE as u32) - 1),
        );
        self.wait_for_erase_or_program(wait)?;

        for (i, row) in
            flash_page.chunks_exact(BYTES_PER_FLASH_WORD).enumerate()
        {
            let row: &[u8; BYTES_PER_FLASH_WORD] = row.try_into().unwrap();

            self.start_write_row(i as u32, row);
            while !self.poll_write_result() {}
        }

        self.start_program(start_word);
        self.wait_for_erase_or_program(wait)?;
        Ok(())
    }

    fn wait_for_erase_or_program(
        &mut self,
        wait: fn() -> (),
    ) -> Result<(), FlashTimeout> {
        loop {
            if let Some(result) = self.poll_erase_or_program_result() {
                return result;
            }

            self.enable_interrupt_sources();
            wait();
            self.disable_interrupt_sources();
        }
    }

    pub fn is_page_range_programmed(&mut self, addr: u32, len: u32) -> bool {
        for i in (addr..addr + len).step_by(BYTES_PER_FLASH_PAGE) {
            let word = i / (BYTES_PER_FLASH_WORD as u32);
            // the blank check is inclusive so we need to end before
            // the last word
            let end_word = (word + WORDS_PER_FLASH_PAGE as u32) - 1;
            self.start_blank_check(word..=end_word);
            loop {
                if let Some(s) = self.poll_blank_check_result() {
                    match s {
                        ProgramState::Blank => return false,
                        _ => break,
                    }
                }
            }
        }

        true
    }
}

/// Raw status bits from the flash controller.
///
/// This type is exposed in case you want to do something weird with the raw
/// flash controller operations; the higher-level APIs do not use it directly.
///
/// Note: this type is only constructed when we find the `DONE` bit set, so the
/// `DONE` bit is not included here.
pub struct Status {
    pub ecc_err: bool,
    pub err: bool,
    pub fail: bool,
}

/// Observed state of a region after a blank check command.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ProgramState {
    Blank,
    NotBlank { word_number: u32 },
}

/// Error produced if the erase or program commands don't terminate as expected
/// by the flash controller. (The timeout is internal and set by hardware.)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FlashTimeout;

/// Internal enumeration of commands understood by the flash controller. This
/// enumeration includes only the variants we use.
///
/// See UM11126 rev2.4 table 171.
#[derive(Copy, Clone, Debug)]
pub enum FlashCmd {
    ReadSingleWord = 3,
    EraseRange = 4,
    BlankCheck = 5,
    Write = 8,
    Program = 12,
}

#[derive(Copy, Clone, Debug)]
pub enum ReadError {
    /// The flash controller rejected the read as illegal, likely because the
    /// word number was out of range.
    IllegalOperation,
    /// The read failed because of an ECC error.
    Ecc,
    /// The read failed due to unspecified other reasons (represents the `FAIL`
    /// bit in the controller).
    Fail,
}
