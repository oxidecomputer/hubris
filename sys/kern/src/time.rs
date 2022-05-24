// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of kernel time.

/// In-kernel timestamp representation.
///
/// This is currently measured in an arbitrary "tick" unit.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct Timestamp(u64);

impl From<u64> for Timestamp {
    fn from(v: u64) -> Self {
        Timestamp(v)
    }
}

impl From<[u32; 2]> for Timestamp {
    fn from(v: [u32; 2]) -> Self {
        Self::from(u64::from(v[0]) | u64::from(v[1]) << 32)
    }
}

impl From<Timestamp> for u64 {
    fn from(v: Timestamp) -> Self {
        v.0
    }
}
