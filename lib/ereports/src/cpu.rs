// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common ereport types from the `hw.pwr.*` class hierarchy.

use fixedstr::FixedString;
use microcbor::{Encode, EncodeFields};

/// An ereport representing an AMD CPU's `THERMTRIP` assertion.
#[derive(Clone, Encode)]
#[ereport(class = "hw.cpu.amd.thermtrip", version = 0)]
pub struct Thermtrip {
    #[cbor(flatten)]
    pub cpu: &'static HostCpuRefdes,
    pub state: crate::pwr::CurrentState,,
}

/// An ereport representing an AMD CPU's `SMERR_L` assertion.
#[derive(Clone, Encode)]
#[ereport(class = "hw.cpu.amd.smerr", version = 0)]
pub struct Smerr {
    #[cbor(flatten)]
    pub cpu: &'static HostCpuRefdes,
    pub state: crate::pwr::CurrentState,
}

/// An ereport representing an unsupported CPU.
#[derive(Clone, Encode)]
#[ereport(class = "hw.cpu.unsup", version = 0)]
pub struct UnsupportedCpu<T: EncodeFields<()>> {
    #[cbor(flatten)]
    pub cpu: &'static HostCpuRefdes,
    #[cbor(flatten)]
    pub cpu_type: T,
}

#[derive(Clone, EncodeFields)]
pub struct HostCpuRefdes {
    /// On both Gimlet and Cosmo, the host CPU's refdes is `P0`.
    pub refdes: FixedString<2>,
    /// As the host CPU's `control-plane-agent` device ID is different from its
    /// refdes, we must include both in the ereport.
    ///
    /// On Gimlet, this is `sp3-host-cpu` and on Cosmo, it is `sp5-host-cpu`.
    //
    // TODO(eliza): It would be cool if we could get this from the same value as
    // where `control-plane-agent` gets it from...but in practice that's
    // annoying.
    pub dev_id: FixedString<12>,
}
