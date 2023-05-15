// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_caboose::CabooseError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::sys_send;

pub use stage0_handoff::ImageVersion;

// RM0433 Rev 7 section 4.3.9
// Flash word is defined as 256 bits
pub const FLASH_WORD_BITS: usize = 256;

// Total length of a word in bytes (i.e. our array size)
pub const FLASH_WORD_BYTES: usize = FLASH_WORD_BITS / 8;

// This is arbitrarily chosen to determine how much data the server will
// process at a time, and is not dictated by the hardware.
pub const FLASH_WORDS_PER_BLOCK: usize = 32;

// Block is an abstract concept here. It represents the size of data the
// driver will process at a time.
pub const BLOCK_SIZE_BYTES: usize = FLASH_WORD_BYTES * FLASH_WORDS_PER_BLOCK;

pub const BLOCK_SIZE_WORDS: usize = BLOCK_SIZE_BYTES / 4;

// TODO(AJS): Fill in details for the SP
#[derive(Copy, Clone, Serialize, Deserialize, SerializedSize)]
pub struct SpUpdateStatus {}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
