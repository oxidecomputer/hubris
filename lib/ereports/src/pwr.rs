// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common ereport types from the `hw.pwr.*` class hierarchy.

use fixedstr::FixedStr;
use microcbor::{Encode, StaticCborLen};

/// An ereport representing a PMBus alert.
#[derive(Clone, Encode)]
#[ereport(class = "hw.pwr.pmbus.alert", version = 0)]
pub struct PmbusAlert<R: StaticCborLen, const REFDES_LEN: usize> {
    pub refdes: FixedStr<'static, REFDES_LEN>,
    pub rail: R,
    pub time: u64,
    pub pwr_good: Option<bool>,
    pub pmbus_status: PmbusStatus,
}

/// An ereport representing a failure to apply the BMR491 firmware mitigation.
#[derive(Clone, Encode)]
#[ereport(class = "hw.pwr.bmr491.mitfail", version = 0)]
pub struct Bmr491MitigationFailure<const REFDES_LEN: usize> {
    pub refdes: FixedStr<'static, REFDES_LEN>,
    pub failures: u32,
    pub last_cause: drv_i2c_devices::bmr491::MitigationFailureKind,
    pub succeeded: bool,
}

/// PMBus status registers.
#[derive(Copy, Clone, Default, Encode)]
pub struct PmbusStatus {
    pub word: Option<u16>,
    pub input: Option<u8>,
    pub iout: Option<u8>,
    pub vout: Option<u8>,
    pub temp: Option<u8>,
    pub cml: Option<u8>,
    pub mfr: Option<u8>,
}

/// Represents the current power state, and how long we have been in that state.
#[derive(Copy, Clone, Encode)]
pub struct CurrentState {
    /// The current CPU power state.
    pub cur: drv_cpu_power_state::PowerState,
    /// The Hubris tick (in milliseconds) at which the transition to this state
    /// occurred.
    pub since: u64,
}
