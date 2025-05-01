// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use bitfield::bitfield;
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use userlib::FromPrimitive;
use zerocopy::{FromBytes, IntoBytes};

use Reg::FAN0_STATE;
bitfield! {
    #[derive(Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, FromBytes)]
    #[repr(C)]
    pub struct FanModuleStatus(u8);
    pub enable, set_enable: FAN0_STATE::ENABLE.trailing_zeros() as usize;
    pub led, set_led: FAN0_STATE::LED.trailing_zeros() as usize;
    pub present, _: FAN0_STATE::PRESENT.trailing_zeros() as usize;
    pub power_good, _: FAN0_STATE::PG.trailing_zeros() as usize;
    pub power_fault, _: FAN0_STATE::POWER_FAULT.trailing_zeros() as usize;
    pub power_timed_out, _: FAN0_STATE::PG_TIMED_OUT.trailing_zeros() as usize;
}

/// Each fan module contains two individually controlled fans
pub const NUM_FAN_MODULES: usize = 4;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FanModuleLedState {
    Off,
    On,
    Blink,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FanModulePowerState {
    Enabled,
    Disabled,
}

/// Four fan modules exist on sidecar, each with two fans.
///
/// The SP applies control at the individual fan level. Power control and
/// status, module presence, and module LED control exist at the module level.
#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive, IntoBytes)]
#[repr(u8)]
pub enum FanModuleIndex {
    Zero = 0,
    One = 1,
    Two = 2,
    Three = 3,
}

pub struct FanModules {
    fpga: FpgaUserDesign,
    presence: [bool; NUM_FAN_MODULES],
    led_state: [FanModuleLedState; NUM_FAN_MODULES],
    power_state: [FanModulePowerState; NUM_FAN_MODULES],
}

impl FanModules {
    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_id,
                MainboardController::DEVICE_INDEX,
            ),
            led_state: [FanModuleLedState::On; NUM_FAN_MODULES],
            presence: [false; NUM_FAN_MODULES],
            power_state: [FanModulePowerState::Enabled; NUM_FAN_MODULES],
        }
    }

    /// Fetch the FANx_STATE register from the FPGA for all fan modules
    ///
    /// Additionally, update internal presence state with the latest status.
    pub fn get_status(
        &mut self,
    ) -> Result<[FanModuleStatus; NUM_FAN_MODULES], FpgaError> {
        let status: [FanModuleStatus; NUM_FAN_MODULES] =
            self.fpga.read(Addr::FAN0_STATE)?;
        self.presence = status.map(|s| s.present());
        Ok(status)
    }

    /// Get the latest fan module presence status for all modules
    pub fn get_presence(&self) -> [bool; NUM_FAN_MODULES] {
        self.presence
    }

    /// Get the presence status for a particular fan module
    pub fn is_present(&self, idx: FanModuleIndex) -> bool {
        self.presence[idx as usize]
    }

    /// Enable the HSC for a fan module
    ///
    /// The FPGA will automatically disable the HSC if a fan module is removed.
    /// The module will need to be re-enabled once it is returned.
    pub fn set_power_state(
        &mut self,
        idx: FanModuleIndex,
        state: FanModulePowerState,
    ) {
        self.power_state[idx as usize] = state;
    }

    pub fn get_power_state(
        &mut self,
        idx: FanModuleIndex,
    ) -> FanModulePowerState {
        self.power_state[idx as usize]
    }

    pub fn update_power(&self) -> Result<(), FpgaError> {
        for (module, state) in self.power_state.iter().enumerate() {
            self.fpga.write(
                if *state == FanModulePowerState::Enabled {
                    WriteOp::BitSet
                } else {
                    WriteOp::BitClear
                },
                Addr::FAN0_STATE as u16 + module as u16,
                Reg::FAN0_STATE::ENABLE,
            )?;
        }
        Ok(())
    }

    /// Get the state of a fan module's LED
    pub fn get_led_state(&self, idx: FanModuleIndex) -> FanModuleLedState {
        self.led_state[idx as usize]
    }

    /// Change the state of a fan module's LED
    ///
    /// This function will only modify the state of an LED if the requested
    /// module is present, otherwise it will keep it off.
    pub fn set_led_state(
        &mut self,
        idx: FanModuleIndex,
        state: FanModuleLedState,
    ) {
        self.led_state[idx as usize] = if self.is_present(idx) {
            state
        } else {
            FanModuleLedState::Off
        };
    }

    /// Turn LEDs on or off depending on their state
    ///
    /// The `blink_on` parameter is external state as to if a blinking LED
    /// should be turned on or off, allowing for synchronization with other
    /// LEDs which may be blinking.
    pub fn update_leds(&self, blink_on: bool) -> Result<(), FpgaError> {
        for (module, state) in self.led_state.iter().enumerate() {
            let module = FanModuleIndex::from_usize(module).unwrap();
            match state {
                FanModuleLedState::Off => self.led_off(module)?,
                FanModuleLedState::On => self.led_on(module)?,
                FanModuleLedState::Blink => self.led_set(module, blink_on)?,
            }
        }
        Ok(())
    }

    // private function to turn an LED on
    fn led_on(&self, idx: FanModuleIndex) -> Result<(), FpgaError> {
        self.led_set(idx, true)
    }

    // private function to turn an LED off
    fn led_off(&self, idx: FanModuleIndex) -> Result<(), FpgaError> {
        self.led_set(idx, false)
    }

    fn led_set(&self, idx: FanModuleIndex, on: bool) -> Result<(), FpgaError> {
        self.fpga.write(
            if on {
                WriteOp::BitSet
            } else {
                WriteOp::BitClear
            },
            Addr::FAN0_STATE as u16 + idx as u16,
            Reg::FAN0_STATE::LED,
        )
    }
}
