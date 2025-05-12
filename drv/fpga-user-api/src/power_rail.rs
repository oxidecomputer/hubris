// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_fpga_api::FpgaError;
use userlib::FromPrimitive;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// This module implements state primitives matching the generic power
/// rail/voltage regulator as implemented in
/// https://github.com/oxidecomputer/quartz/blob/main/hdl/power_rail.rdl,
/// https://github.com/oxidecomputer/quartz/blob/main/hdl/PowerRail.bsv.

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    FromBytes,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct RawPowerRailState(u8);

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct PowerRail {
    pub status: PowerRailStatus,
    pub pins: PowerRailPinState,
}

impl TryFrom<RawPowerRailState> for PowerRail {
    type Error = FpgaError;

    fn try_from(raw_state: RawPowerRailState) -> Result<Self, Self::Error> {
        Ok(Self {
            status: PowerRailStatus::try_from(raw_state)?,
            pins: PowerRailPinState::from(raw_state),
        })
    }
}

/// Status type for the power rail. This type mirrors the State type in
/// https://github.com/oxidecomputer/quartz/blob/main/hdl/PowerRail.bsv
/// and should be kept in sync.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum PowerRailStatus {
    #[default]
    Disabled = 0,
    RampingUp = 1,
    GoodTimeout = 2,
    Aborted = 3,
    Enabled = 4,
}

impl TryFrom<RawPowerRailState> for PowerRailStatus {
    type Error = FpgaError;

    fn try_from(raw_state: RawPowerRailState) -> Result<Self, Self::Error> {
        Self::from_u8(raw_state.0 >> 4).ok_or(FpgaError::InvalidValue)
    }
}

/// Type representing the pin state of a generic voltage regulator. This struct
/// and the `TryFrom` implementation should be kept in sync with
/// https://github.com/oxidecomputer/quartz/blob/main/hdl/power_rail.rdl.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct PowerRailPinState {
    pub enable: bool,
    pub good: bool,
    pub fault: bool,
    pub vrhot: bool,
}

impl From<RawPowerRailState> for PowerRailPinState {
    fn from(raw_state: RawPowerRailState) -> Self {
        Self {
            enable: raw_state.0 & (1 << 0) != 0,
            good: raw_state.0 & (1 << 1) != 0,
            fault: raw_state.0 & (1 << 2) != 0,
            vrhot: raw_state.0 & (1 << 3) != 0,
        }
    }
}
