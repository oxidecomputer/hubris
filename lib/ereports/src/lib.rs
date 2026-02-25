// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ereport message definitions shared between multiple tasks.

#![no_std]

pub mod cpu;
pub mod pwr;

/// A wrapper adding a device ID field to an ereport. This is to be used when an
/// ereport refers to a device where the `control-plane-agent` device ID is
/// different from the refdes of the device.
#[derive(Clone, microcbor::Encode)]
pub struct WithDevId<E: EncodeFields<()>, const DEVID_LEN: usize> {
    pub dev_id: FixedStr<'static, DEVID_LEN>,
    #[cbor(flatten)]
    pub ereport: E,
}
