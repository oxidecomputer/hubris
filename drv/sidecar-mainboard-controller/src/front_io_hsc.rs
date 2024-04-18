// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign};
pub use drv_fpga_user_api::power_rail::*;

pub struct HotSwapController {
    fpga: FpgaUserDesign,
}

impl HotSwapController {
    pub fn new(task_port: userlib::TaskId) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_port,
                MainboardController::DEVICE_INDEX,
            ),
        }
    }

    #[inline]
    fn raw_state(&self) -> Result<RawPowerRailState, FpgaError> {
        self.fpga.read(Addr::FRONT_IO_STATE)
    }

    #[inline]
    pub fn status(&self) -> Result<PowerRailStatus, FpgaError> {
        PowerRailStatus::try_from(self.raw_state()?)
    }

    pub fn set_enable(&self, enable: bool) -> Result<(), FpgaError> {
        self.fpga.write(
            enable.into(),
            Addr::FRONT_IO_STATE,
            Reg::FRONT_IO_STATE::ENABLE,
        )
    }
}
