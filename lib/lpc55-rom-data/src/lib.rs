// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! lpc55-rom-data
//!
//! This crate is intended as a home for data / types / constants that need
//! to be usable on both the host and target. When various data from the
//! lpc55-romapi crate is needed in software built for the host it should
//! be moved here.

#![no_std]

pub const FLASH_PAGE_SIZE: usize = 512;
