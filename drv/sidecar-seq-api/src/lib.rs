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
        DebugPortState, DirectBarSegment, SpiEepromInstruction,
        TofinoCfgRegisters, TofinoPcieReset, TofinoPowerRail, TofinoSeqError,
        TofinoSeqState, TofinoSeqStep,
    },
};

use drv_sidecar_mainboard_controller::tofino2::TofinoBar0Registers;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum SeqError {
    FpgaError = 1,
    IllegalTransition,
    ClockConfigurationFailed,
    SequencerError,
    SequencerTimeout,
    InvalidTofinoVid,
    SetVddCoreVoutFailed,
    NoFrontIOBoard,
    FrontIOBoardPowerFault,
    FrontIOPowerNotGood,

    #[idol(server_death)]
    ServerRestarted,
}

impl From<FpgaError> for SeqError {
    fn from(_: FpgaError) -> Self {
        Self::FpgaError
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
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

pub use drv_sidecar_mainboard_controller::fan_modules::FanModuleIndex;

// Wrapper for debugging because it's very messy to go from enum -> u32
// in a constant array
#[derive(Copy, Clone)]
pub enum TofinoPcieRegs {
    Bar0(TofinoBar0Registers),
    Cfg(TofinoCfgRegisters),
}

impl From<TofinoPcieRegs> for u32 {
    fn from(r: TofinoPcieRegs) -> Self {
        match r {
            TofinoPcieRegs::Bar0(b) => b.into(),
            TofinoPcieRegs::Cfg(c) => c.into(),
        }
    }
}

pub const TOFINO_DEBUG_REGS: [(DirectBarSegment, TofinoPcieRegs); 2] = [
    (
        DirectBarSegment::Bar0,
        TofinoPcieRegs::Bar0(TofinoBar0Registers::PcieDevInfo),
    ),
    (
        DirectBarSegment::Bar0,
        TofinoPcieRegs::Bar0(TofinoBar0Registers::SoftwareReset),
    ),
];

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
