// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Ignition server.

#![no_std]

use bitfield::bitfield;
use derive_idol_err::IdolError;
use drv_fpga_api::FpgaError;
use idol_runtime::ServerDeath;
use userlib::{sys_send, FromPrimitive, ToPrimitive};
use zerocopy::{AsBytes, FromBytes};

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, IdolError,
)]
pub enum IgnitionError {
    ServerDied,
    FpgaError,
    InvalidPort,
    InvalidValue,
    NoTargetPresent,
    RequestInProgress,
}

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

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct PortState(u64);
    pub target_present, _: 0;
}

impl PortState {
    pub fn target(&self) -> Option<Target> {
        if self.target_present() {
            Some(Target(self.0 & 0xffffffffffff0000))
        } else {
            None
        }
    }

    pub fn receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus((self.0 >> 8) as u8)
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct ReceiverStatus(u8);
    pub aligned, _: 0;
    pub locked, _: 1;
    pub polarity_inverted, _: 2;
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct Target(u64);
    pub controller0_present, _: 24;
    pub controller1_present, _: 25;
    pub system_power_abort, _: 27;
    pub system_power_fault_a3, _: 32;
    pub system_power_fault_a2, _: 33;
    pub reserved_fault1, _: 34;
    pub reserved_fault2, _: 35;
    pub sp_fault, _: 36;
    pub rot_fault, _: 37;
    pub system_power_off_in_progress, _: 40;
    pub system_power_on_in_progress, _: 41;
    pub system_reset_in_progress, _: 42;
}

impl Target {
    pub fn system_type(&self) -> SystemType {
        SystemType(self.0.as_bytes()[2])
    }

    pub fn system_power_state(&self) -> PowerState {
        match self.0.as_bytes()[3] & 0x4 != 0 {
            true => PowerState::On,
            false => PowerState::Off,
        }
    }

    pub fn link0_receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus(self.0.as_bytes()[6])
    }

    pub fn link1_receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus(self.0.as_bytes()[7])
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    FromPrimitive,
    ToPrimitive,
    FromBytes,
    AsBytes,
)]
#[repr(C)]
pub struct SystemType(pub u8);

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum PowerState {
    Off = 0,
    On = 1,
}

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum Request {
    SystemPowerOff = 1,
    SystemPowerOn = 2,
    SystemReset = 3,
}

impl From<Request> for u8 {
    fn from(r: Request) -> Self {
        r as u8
    }
}

#[derive(Copy, Clone, Debug, PartialEq, AsBytes, FromBytes)]
#[repr(C)]
pub struct Counters {
    status_received: u8,
    hello_sent: u8,
    request_sent: u8,
    messages_dropped: u8,
}

impl Default for Counters {
    fn default() -> Self {
        Counters {
            status_received: 0,
            hello_sent: 0,
            request_sent: 0,
            messages_dropped: 0,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
