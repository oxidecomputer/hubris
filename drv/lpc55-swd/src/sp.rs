// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//

// Stuff we know about the SP and its Hubris images but would rather get
// without copying it here.
// We could extract these values from a representive SP hubris ELF file.
pub const IMAGE_HEADER_OFFSET: u32 = 0x298;
pub const IMAGE_HEADER_MAGIC_OFFSET: u32 = IMAGE_HEADER_OFFSET + 0;
pub const IMAGE_HEADER_LENGTH_OFFSET: u32 = IMAGE_HEADER_OFFSET + 4;

// Use an arbitrary minimum size. Images less than this will not be measured.
pub const MIN_IMAGE_SIZE: usize = 0x10000;
