// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! QSPI constants used by the QSPI driver and its users.

#![no_std]

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
pub const SECTOR_SIZE_BYTES: usize = 65_536;

pub enum Command {
    ReadStatusReg = 0x05,
    WriteEnable = 0x06,
    PageProgram = 0x12,
    Read = 0x13,

    // Note, There are multiple ReadId commands.
    // Gimlet and Gemini's flash parts both respond to 0x9F.
    // Gemini's does not respond to 0x9E (returns all zeros).
    // TODO: Proper flash chip quirk support.
    ReadId = 0x9F,

    BulkErase = 0xC7,
    SectorErase = 0xDC,
}

impl From<Command> for u8 {
    fn from(c: Command) -> u8 {
        c as u8
    }
}
