// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_fpga_api::{FpgaError, FpgaUserDesign};
use drv_fpga_user_api::power_rail::{PowerRailStatus, RawPowerRailState};
use drv_front_io_api::FrontIOError;
use drv_sidecar_mainboard_controller::{Addr, MainboardController, Reg};
use userlib::task_slot;

task_slot!(MAINBOARD, mainboard);

pub struct Bsp {
    fpga: FpgaUserDesign,
}

impl Bsp {
    pub fn new() -> Result<Self, FrontIOError> {
        Ok(Self {
            fpga: FpgaUserDesign::new(
                MAINBOARD.get_task_id(),
                MainboardController::DEVICE_INDEX,
            ),
        })
    }

    #[inline]
    fn raw_state(&self) -> Result<RawPowerRailState, FpgaError> {
        self.fpga.read(Addr::FRONT_IO_STATE)
    }

    #[inline]
    pub fn status(&self) -> Result<PowerRailStatus, FpgaError> {
        PowerRailStatus::try_from(self.raw_state()?)
    }

    pub fn power_good(&self) -> Result<bool, FrontIOError> {
        match self.status().map_err(FrontIOError::from)? {
            PowerRailStatus::Enabled => Ok(true),
            PowerRailStatus::Disabled | PowerRailStatus::RampingUp => Ok(false),
            PowerRailStatus::GoodTimeout | PowerRailStatus::Aborted => {
                Err(FrontIOError::PowerFault)
            }
        }
    }

    pub fn set_power_enable(&self, enable: bool) -> Result<(), FrontIOError> {
        self.fpga
            .write(
                enable.into(),
                Addr::FRONT_IO_STATE,
                Reg::FRONT_IO_STATE::ENABLE,
            )
            .map_err(FrontIOError::from)
    }
}
