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
//! The valid measurement is retrieved and recorded if available.
//!

#![no_std]

use sha3::digest::{core_api::OutputSizeUser, typenum::Unsigned};
use sha3::Sha3_256;
use zerocopy::*;

pub const DIGEST_SIZE: usize = <Sha3_256 as OutputSizeUser>::OutputSize::USIZE;

#[derive(Copy, Clone)]
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
    pub digest: [u8; DIGEST_SIZE],
}

impl Shared {
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
