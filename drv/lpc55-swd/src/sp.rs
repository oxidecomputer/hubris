// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//

// Stuff we know about the SP and its Hubris images but would rather get
// without copying it here.
// We could extract these values from a representive SP hubris ELF file.
pub const FLASH_START: u32 = 0x0800_0000;
pub const FLASH_END: u32 = 0x0810_1000;
pub const IMAGE_HEADER_ADDR: u32 = FLASH_START + 0x298;
pub const IMAGE_HEADER_MAGIC_ADDR: u32 = IMAGE_HEADER_ADDR + 0;
pub const IMAGE_HEADER_LENGTH_ADDR: u32 = IMAGE_HEADER_ADDR + 4;
pub const MIN_IMAGE_SIZE: usize = 0x10000; // An arbitrary minimum
pub const FLASH_SIZE: usize = (FLASH_END - FLASH_START) as usize;
