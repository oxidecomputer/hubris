// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common definitions for dealing with system state on Gimlet, shared by the
//! various tasks and IPC interfaces involved.

#![no_std]

use userlib::FromPrimitive;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    PartialEq,
    Eq,
    IntoBytes,
    Immutable,
    KnownLayout,
    counters::Count,
)]
#[repr(u8)]
pub enum PowerState {
    /// Initial A2 state where the SP and most associated circuitry is powered.
    A2 = 1,
    /// A2 substate where we've turned on the fan hotplug controller.
    A2PlusFans = 3,
    // Intermediate A1 state on the way toward A0. This corresponds to the
    // system-wide notion of A1 and currently has no substates. We never
    // broadcast this state outside of the sequencer.
    // A1 = 4,
    /// Initial A0 state: the system-wide A0 domain is on, but we have not
    /// turned on any of the subdomains within A0 (below).
    A0 = 5,
    /// A0 with the NIC hotplug controller enabled. This is the state we expect
    /// to be in most of the time.
    A0PlusHP = 6,

    /// A thermal trip event has occurred in A0. This state is terminal and
    /// requires an explicit transition back to A2.
    A0Thermtrip = 7,

    /// We have detected a host reset in A0. This state is terminal and requires
    /// an explicit transition back to A2.
    A0Reset = 8,
}
