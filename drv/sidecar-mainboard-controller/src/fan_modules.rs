// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use bitfield::bitfield;
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use userlib::FromPrimitive;
use zerocopy::{AsBytes, FromBytes};

bitfield! {
    #[derive(Copy, Clone, PartialEq, Eq, FromPrimitive, AsBytes, FromBytes)]
    #[repr(C)]
    pub struct FanModuleStatus(u8);
    pub enable, set_enable: 0;
    pub led, set_led: 1;
    pub present, _: 2;
    pub power_good, _: 3;
    pub power_fault, _: 4;
    pub power_timed_out, _: 5;
}

/// Each fan module contains two individually controlled fans
pub const NUM_FAN_MODULES: usize = 4;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FanModuleLedState {
    Off = 0,
    On = 1,
    Blink = 2,
}

pub struct FanModules {
    fpga: FpgaUserDesign,
    presence: [bool; NUM_FAN_MODULES],
    led_state: [FanModuleLedState; NUM_FAN_MODULES],
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
        for (module, status) in status.iter().enumerate() {
            self.presence[module] = status.present();
        }
        Ok(status)
    }

    /// Get the latest fan module presence status for all modules
    pub fn get_presence(&self) -> [bool; NUM_FAN_MODULES] {
        self.presence
    }

    /// Get the presence status for a particular fan module
    pub fn is_present(&self, idx: u8) -> bool {
        self.presence[idx as usize]
    }

    /// Enable the HSC for a fan module
    ///
    /// The FPGA will automatically disable the HSC if a fan module is removed.
    /// The module will need to be re-enabled once it is returned.
    pub fn set_enable(&self, idx: u8) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::BitSet,
            Addr::FAN0_STATE as u16 + idx as u16,
            Reg::FAN0_STATE::ENABLE,
        )?;
        Ok(())
    }

    /// Disable the HSC for a fan module
    ///
    /// The FPGA will automatically disable the HSC if a fan module is removed.
    /// The module will need to be re-enabled once it is returned.
    pub fn clear_enable(&self, idx: u8) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::BitClear,
            Addr::FAN0_STATE as u16 + idx as u16,
            Reg::FAN0_STATE::ENABLE,
        )?;
        Ok(())
    }

    /// Get the state of a fan module's LED
    pub fn get_led_state(&self, idx: u8) -> FanModuleLedState {
        self.led_state[idx as usize]
    }

    /// Change the state of a fan module's LED
    ///
    /// This function will only modify the state of an LED if the requested
    /// module is present, otherwise it will keep it off.
    pub fn set_led_state(&mut self, idx: u8, state: FanModuleLedState) {
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
            match state {
                FanModuleLedState::Off => self.led_off(module as u8)?,
                FanModuleLedState::On => self.led_on(module as u8)?,
                FanModuleLedState::Blink => {
                    if blink_on {
                        self.led_on(module as u8)?
                    } else {
                        self.led_off(module as u8)?
                    }
                }
            }
        }
        Ok(())
    }

    // private function to turn an LED on
    fn led_on(&self, idx: u8) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::BitSet,
            Addr::FAN0_STATE as u16 + idx as u16,
            Reg::FAN0_STATE::LED,
        )?;
        Ok(())
    }

    // private function to turn an LED off
    fn led_off(&self, idx: u8) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::BitClear,
            Addr::FAN0_STATE as u16 + idx as u16,
            Reg::FAN0_STATE::LED,
        )?;
        Ok(())
    }
}
