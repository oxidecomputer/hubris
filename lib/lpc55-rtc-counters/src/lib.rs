// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use lpc55_pac as device;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde::{Deserialize, Serialize};

pub struct RtcCounter {
    reg: &'static device::rtc::RegisterBlock,
}

impl From<&'static device::rtc::RegisterBlock> for RtcCounter {
    fn from(reg: &'static device::rtc::RegisterBlock) -> Self {
        // From 28.6.1 of v2.5 of UM11126, this bit must be
        // cleared before doing anything else
        reg.ctrl.modify(|_, w| w.swreset().clear_bit());
        Self { reg }
    }
}

#[repr(u32)]
enum Counter {
    // Tracks the state of the Hubris image
    HubrisBootState = 0,
    // Incremented on every boot
    BootCount = 1,
    // Incremented when the sytem has rebooted in an unexpected way
    RebootCount = 2,
    // Incremented when a fault is taken in the secure world
    SecureFaultCount = 3,
}

#[derive(Debug, FromPrimitive, Serialize, Deserialize, Copy, Clone)]
#[repr(u32)]
pub enum HubrisState {
    // We have no information about the last Hubris boot. This corresponds to
    // the value of the register after a power on reset. May be returned if
    // the register contains other unknown values
    Unknown = 0,
    // The hubris image with the higher version is able to receive updates from
    // the SP over SPI
    Ready = 1,
    // An update to the specified slot has started. We cannot make any
    // guarantees about the image when in this state.
    UpdateStartA = 3,
    UpdateStartB = 4,
    // Update has completed. The image can now be booted according to
    // selected version
    UpdatePending = 5,
    // First attempt at booting
    FirstBoot = 6,
    // Higher level software has declared it good
    Committed = 7,
}

impl RtcCounter {
    pub fn increment_boot_count(&mut self) {
        let val = self.read_counter(Counter::BootCount);
        self.write_counter(Counter::BootCount, val + 1);
    }

    pub fn increment_secure_fault(&mut self) {
        let val = self.read_counter(Counter::SecureFaultCount);
        self.write_counter(Counter::SecureFaultCount, val + 1);
    }

    pub fn increment_reboot(&mut self) {
        let val = self.read_counter(Counter::RebootCount);
        self.write_counter(Counter::RebootCount, val + 1);
    }

    pub fn set_hubris_state(&mut self, state: HubrisState) {
        self.write_counter(Counter::HubrisBootState, state as u32)
    }

    pub fn get_hubris_state(&self) -> HubrisState {
        match HubrisState::from_u32(self.read_counter(Counter::HubrisBootState))
        {
            Some(a) => a,
            None => HubrisState::Unknown,
        }
    }

    fn read_counter(&self, counter: Counter) -> u32 {
        self.reg.gpreg[counter as usize].read().bits()
    }

    fn write_counter(&mut self, counter: Counter, val: u32) {
        unsafe { self.reg.gpreg[counter as usize].write(|w| w.bits(val)) }
    }
}
