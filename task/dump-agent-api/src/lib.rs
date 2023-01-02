// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Dump Agent task.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum DumpAgentError {
    InvalidArea = 1,
    BadOffset = 2,
    UnalignedOffset = 3,
    UnalignedSegmentAddress = 4,
    UnalignedSegmentLength = 5,
}

#[derive(Copy, Clone, Debug, SerializedSize, Serialize, Deserialize)]
pub struct DumpArea {
    pub address: u32,
    pub length: u32,
}

pub const DUMP_MAGIC: [u8; 4] = [ 0x1, 0xde, 0xde, 0xad ];
pub const DUMP_SEGMENT_PAD: u8 = 0x55;
pub const DUMP_REGISTER_MAGIC: [u8; 2] = [ 0xab, 0xba ];
pub const DUMP_READ_SIZE: usize = 256;
pub const DUMP_AGENT_VERSION: u8 = 1_u8;

#[derive(Copy, Clone, Debug, AsBytes)]
#[repr(C, packed)]
pub struct DumpAreaHeader {
    /// Magic to indicate that this is an area header
    pub magic: [u8; 4],

    /// Version of dump agent 
    pub agent_version: u8,

    /// Version of dumper (to be written by dumper)
    pub dumper_version: u8,

    /// Number of segment headers that follow this header
    pub nsegments: u16,

    /// Length of this area
    pub length: u32,

    /// Total bytes that have been actually written in this area,
    /// including all headers
    pub written: u32,
}

#[derive(Copy, Clone, Debug, AsBytes)]
#[repr(C, packed)]
pub struct DumpSegmentHeader {
    pub address: u32,
    pub length: u32,
}

//
// A segment of actual data, as stored by the dumper into the dump area(s).  Note
// that we very much depend on endianness here:  any unused space at the end of
// of a single area will be filled with DUMP_SEGMENT_PAD.
//
#[derive(Copy, Clone, Debug, AsBytes)]
#[repr(C, packed)]
#[cfg(target_endian = "little")]
pub struct DumpSegmentData {
    pub address: u32,
    pub length: u16,
    pub actual: u16,
}

#[derive(Copy, Clone, Debug, AsBytes)]
#[repr(C, packed)]
pub struct DumpRegister {
    /// Register magic -- must be DUMP_REGISTER_MAGIC
    pub magic: u16,

    /// Name of register
    pub register: u16,

    /// Value of register
    pub val: u32,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
