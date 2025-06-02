// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Minibar Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;

use userlib::*;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum MinibarSeqError {
    FpgaError = 1,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for MinibarSeqError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

mod reg_map {
    include!(concat!(env!("OUT_DIR"), "/minibar_regs.rs"));
}

// exporting for ease of use with minibar-ignition-server and minibar-seq-server
pub use crate::reg_map::MINIBAR_BITSTREAM_CHECKSUM;
pub use reg_map::Addr;
pub use reg_map::Reg;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
