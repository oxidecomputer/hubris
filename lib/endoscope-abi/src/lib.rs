// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! endoscope-abi
//!
//! This crate documents the interface of the program that the RoT injects into
//! the SP that measures the SP flash contents.
//!
//! When SP reset is detected, the RoT halts the SP, injects the endoscope program
//! into the SP RAM and runs it. The endoscope program will measure the entire
//! active flash bank and deposit the resulting Sha3-256 hash into the Shared
//! structure. The RoT polls the STM32 debug module waiting for a halt or timeout.
//! The valid measurement is retrieved and recorded if availalble.
//!

#![no_std]

use zerocopy::*;

#[repr(u32)]
pub enum State {
    #[allow(dead_code)]
    Preboot = 0,
    Running = 0x1de6060,
    Done = 0x1dec1a0,
}

#[derive(FromBytes, AsBytes, Copy, Clone)]
#[repr(C, packed)]
pub struct Shared {
    pub state: u32,
    pub digest: [u8; 256 / 8],
}

impl Shared {
    pub const MAGIC: u32 = 0x1de2019;
    pub const STATE_PREBOOT: u32 = 0;
    /// The program main routine has started.
    pub const STATE_RUNNING: u32 = 0x1de6060;
    /// The program main routine finished. Shared::digest is valid.
    pub const STATE_DONE: u32 = 0x1dec1a0;

    pub fn parse(bytes: &[u8]) -> Option<&Self> {
        if let Some((layout, _)) =
            LayoutVerified::<&[u8], Self>::new_from_prefix(bytes)
        {
            let shared: &Shared = layout.into_ref();
            Some(shared)
        } else {
            None
        }
    }
}

// Symbols relied on in the endoscope.elf file.
// The image load address.
pub const LOAD_SYMBOL: &str = "__vector_table";
// An instance of struct Shared is expected at this address
pub const SHARED_STRUCT_SYMBOL: &str = "SHARED";
// The reset vector found in the image should match this symbol value.
pub const RESET_VECTOR_SYMBOL: &str = "Reset";
