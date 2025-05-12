// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Sequencer server.

#![no_std]

use counters::Count;
use derive_idol_err::IdolError;
use userlib::{sys_send, FromPrimitive};
use zerocopy::{Immutable, IntoBytes, KnownLayout};

// Re-export PowerState for client convenience.
pub use drv_cpu_power_state::PowerState;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, Count,
)]
pub enum SeqError {
    IllegalTransition = 1,
    MuxToHostCPUFailed,
    MuxToSPFailed,
    CPUNotPresent,
    UnrecognizedCPU,
    A1Timeout,
    A0TimeoutGroupC,
    A0Timeout,
    I2cFault,

    #[idol(server_death)]
    ServerRestarted,
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
    Count,
)]
#[repr(u8)]
pub enum StateChangeReason {
    /// No reason was provided.
    ///
    /// This indicates a legacy caller of `Sequencer.set_state`, rather than
    /// `Sequencer.set_state_with_reason`. All Hubris-internal callers should
    /// use `set_state_with_reason`, so this variant generally indicates that
    /// the `Sequencer.set_state` IPC is being called via Hiffy.
    Other = 1,
    /// The system has just received power, so the sequencer has booted the
    /// host CPU.
    InitialPowerOn,
    /// A power state change was requested by the control plane.
    ControlPlane,
    /// The host CPU reset while in A0, so the system has powered off to clear
    /// hidden core state.
    CpuReset,
    /// The host OS failed to boot, so the system has powered off.
    HostBootFailure,
    /// The host OS panicked.
    HostPanic,
    /// The host OS requested that the system power off without rebooting.
    HostPowerOff,
    /// The host OS requested that the system reboot.
    HostReboot,
    /// The system powered off because a component has overheated.
    Overheat,
}

/// Indicates the result of a power state transition.
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
    Count,
)]
#[repr(u8)]
pub enum Transition {
    /// The sequencer successfully transitioned to the desired state.
    Done,
    /// The sequencer believes the system was already in the desired state, and
    /// did not perform a state transition.
    NoChange,
}

// On Gimlet, we have two banks of up to 8 DIMMs apiece. Export the "two banks"
// bit of knowledge here so it can be used by gimlet-seq-server, spd, and
// packrat, all of which want to know at compile-time how many banks there are.
pub const NUM_SPD_BANKS: usize = 2;

use crate as drv_cpu_seq_api;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
