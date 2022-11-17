// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Ignition server.

#![no_std]

use bitfield::bitfield;
use derive_idol_err::IdolError;
use derive_more::From;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
use zerocopy::{AsBytes, FromBytes};

// The `presence_summary` vector (see `ignition-server`) is implicitly capped at 40 bits by (the RTL of
// the) mainboard controller. This constant is used to conservatively allocate
// an array type which can contain the port state for all ports. The actual
// number of pors configured in the system can be learned through the
// `port_count()` function below.
pub const PORT_MAX: usize = 40;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    ToPrimitive,
    IdolError,
)]
pub enum IgnitionError {
    ServerDied = 1,
    FpgaError,
    InvalidPort,
    InvalidValue,
    NoTargetPresent,
    RequestInProgress,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, FromPrimitive, From, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct PortState(u64);
    pub target_present, _: 0;
    pub u8, into ReceiverStatus, receiver_status, _: 15, 8;
    u64, into Target, raw_target, _: 63, 16;
}

impl PortState {
    pub fn target(&self) -> Option<Target> {
        if self.target_present() {
            Some(self.raw_target())
        } else {
            None
        }
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, From, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct ReceiverStatus(u8);
    pub aligned, _: 0;
    pub locked, _: 1;
    pub polarity_inverted, _: 2;
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, From, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct SystemFaults(u8);
    pub power_a3, _: 0;
    pub power_a2, _: 1;
    pub reserved1, _: 2;
    pub reserved2, _: 3;
    pub sp, _: 4;
    pub rot, _: 5;
}

impl SystemFaults {
    pub fn count(&self) -> usize {
        self.0.count_ones().try_into().unwrap()
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, From, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct Target(u64);
    pub u8, into SystemType, system_type, _: 7, 0;
    pub controller0_present, _: 8;
    pub controller1_present, _: 9;
    raw_system_power_state, _: 10;
    pub system_power_abort, _: 11;
    pub u8, into SystemFaults, faults, _: 23, 16;
    pub system_power_off_in_progress, _: 24;
    pub system_power_on_in_progress, _: 25;
    pub system_power_reset_in_progress, _: 26;
    pub u8, into ReceiverStatus, link0_receiver_status, _: 39, 32;
    pub u8, into ReceiverStatus, link1_receiver_status, _: 47, 40;
}

impl Target {
    #[inline]
    pub fn system_power_state(&self) -> PowerState {
        if self.raw_system_power_state() {
            PowerState::On
        } else {
            PowerState::Off
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, From, FromBytes, AsBytes)]
#[repr(C)]
pub struct SystemType(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Eq, From, AsBytes)]
#[repr(u8)]
pub enum PowerState {
    Off = 0,
    On = 1,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, From, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum Request {
    SystemPowerOff = 1,
    SystemPowerOn = 2,
    SystemPowerReset = 3,
}

impl From<Request> for u8 {
    fn from(r: Request) -> Self {
        r as u8
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, AsBytes, FromBytes)]
#[repr(C)]
pub struct Counters {
    status_received: u8,
    hello_sent: u8,
    request_sent: u8,
    messages_dropped: u8,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct LinkEvents(u8);
    encoding_error, _: 0;
    decoding_error, _: 1;
    ordered_set_invalid, _: 2;
    message_version_invalid, _: 3;
    message_type_invalid, _: 4;
    message_checksum_invalid, _: 5;
}

impl LinkEvents {
    pub const NONE: Self = Self(0b000000);
    pub const ALL: Self = Self(0b111111);
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, From, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum LinkSelect {
    Controller = 1,
    TargetLink0 = 2,
    TargetLink1 = 3,
}

cfg_if::cfg_if! {
    if #[cfg(feature = "idol-client")] {
        use drv_fpga_api::FpgaError;
        use idol_runtime::ServerDeath;
        use userlib::sys_send;

        impl From<ServerDeath> for IgnitionError {
            fn from(_e: ServerDeath) -> Self {
                Self::ServerDied
            }
        }

        impl From<FpgaError> for IgnitionError {
            fn from(_e: FpgaError) -> Self {
                Self::FpgaError
            }
        }

        include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
    }
}
