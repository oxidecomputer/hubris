// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Sidecar Sequencer server.

#![no_std]

use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
pub use drv_sidecar_mainboard_controller::{
    fan_modules::{FanModuleStatus, NUM_FAN_MODULES},
    tofino2::{
        DebugPortState, DirectBarSegment, PowerRail, SpiEepromInstruction,
        TofinoPcieReset, TofinoSeqError, TofinoSeqState, TofinoSeqStep,
    },
};

use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
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

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct FanModulePresence(pub [bool; NUM_FAN_MODULES]);

/// Four fan modules exist on sidecar, each with two fans.
///
/// The SP applies control at the individual fan level. Power control and
/// status, module presence, and module LED control exist at the module level.
#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum FanModuleIndex {
    Zero = 0,
    One = 1,
    Two = 2,
    Three = 3,
}

impl From<u8> for FanModuleIndex {
    fn from(v: u8) -> Self {
        match v {
            0 => FanModuleIndex::Zero,
            1 => FanModuleIndex::One,
            2 => FanModuleIndex::Two,
            3 => FanModuleIndex::Three,
            _ => panic!(), // invalid fan module index
        }
    }
}

impl From<usize> for FanModuleIndex {
    fn from(v: usize) -> Self {
        FanModuleIndex::from(v as u8)
    }
}

impl From<FanModuleIndex> for u8 {
    fn from(v: FanModuleIndex) -> u8 {
        match v {
            FanModuleIndex::Zero => 0,
            FanModuleIndex::One => 1,
            FanModuleIndex::Two => 2,
            FanModuleIndex::Three => 3,
        }
    }
}

impl From<FanModuleIndex> for usize {
    fn from(v: FanModuleIndex) -> usize {
        u8::from(v) as usize
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
