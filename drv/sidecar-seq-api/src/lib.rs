// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Sidecar Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
pub use drv_sidecar_mainboard_controller::tofino2::{
    DebugPortState, DirectBarSegment, PowerRail, SpiEepromInstruction,
    TofinoPcieReset, TofinoSeqError, TofinoSeqState, TofinoSeqStep,
};
use userlib::*;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum SeqError {
    FpgaError = 1,
    IllegalTransition,
    ClockConfigurationFailed,
    SequencerError,
    SequencerTimeout,
    InvalidTofinoVid,
    SetVddCoreVoutFailed,
    NoFrontIOBoard,
    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for SeqError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, AsBytes)]
#[repr(u8)]
pub enum TofinoSequencerPolicy {
    Disabled = 0,
    LatchOffOnFault = 1,
    RestartOnFault = 2,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
