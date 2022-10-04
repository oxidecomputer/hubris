// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for QSFP transceiver managment

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use userlib::{sys_send, FromPrimitive};

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum TransceiversError {
    FpgaError = 1,
    InvalidPortNumber = 2,
    InvalidNumberOfBytes = 3,
}

impl From<FpgaError> for TransceiversError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

/// Each field is a bitmask of the 32 transceivers in big endian order, which
/// results in Port 31 being bit 31, and so forth.
#[derive(Copy, Clone, zerocopy::FromBytes, zerocopy::AsBytes)]
#[repr(C)]
pub struct ModulesStatus {
    pub enable: u32,
    pub reset: u32,
    pub lpmode_txdis: u32,
    pub power_good: u32,
    pub power_good_timeout: u32,
    pub present: u32,
    pub irq_rxlos: u32,
}

/// Size in bytes of a page section we will read or write
///
/// QSFP module's internal memory map is 256 bytes, with the lower 128 being
/// static and then the upper 128 are paged in. The internal address register
/// is only 7 bits, so you can only access half in any single transaction and
/// thus our communication mechanisms have been designed for that.
/// See SFF-8636 and CMIS specifications for details.
pub const PAGE_SIZE_BYTES: usize = 128;

/// The only instantiation of Front IO board that exists is one with 32 QSFP
/// ports.
pub const NUM_PORTS: u8 = 32;

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
