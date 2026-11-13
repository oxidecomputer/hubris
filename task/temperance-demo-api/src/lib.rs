// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

#![no_std]

use postcard_schema_ng::Schema;
pub use postcard_schema_ng::max_len::MaxLenByteSlice;
use serde::{Deserialize, Serialize};
use temperance::interface;

/// TODO: Add docs
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct AddRequest {
    /// TODO: Add docs
    pub a: u32,
    /// TODO: Add docs
    pub b: u32,
}

/// TODO: Add docs
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub enum AddError {
    /// TODO: Add docs
    Overflow,
}

/// TODO: Add docs
pub const CHECKSUM_MAX_LEN: usize = 32;

/// TODO: Add docs
pub const BULK_MAX_LEN: usize = 512;

/// TODO: Add docs
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Schema)]
pub struct FillRequest {
    /// TODO: Add docs
    pub byte: u8,
    /// TODO: Add docs
    pub len: u16,
}

interface! {
    /// TODO: Add docs
    mod demo {
        /// TODO: Add docs
        fn add(AddRequest) -> Result<u32, AddError>;
        /// TODO: Add docs
        fn bump(u32) -> u64;
        /// TODO: Add docs
        fn checksum(MaxLenByteSlice<'rqst, CHECKSUM_MAX_LEN>) -> u32;
    }
}

interface! {
    /// TODO: Add docs
    mod demo_upload {
        /// TODO: Add docs
        fn sum(MaxLenByteSlice<'rqst, BULK_MAX_LEN>) -> u32;
    }
}

interface! {
    /// TODO: Add docs
    mod demo_bulk {
        /// TODO: Add docs
        fn fill(FillRequest) -> MaxLenByteSlice<'resp, BULK_MAX_LEN>;
        /// TODO: Add docs
        fn echo(MaxLenByteSlice<'rqst, BULK_MAX_LEN>)
            -> MaxLenByteSlice<'resp, BULK_MAX_LEN>;
    }
}
