// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
pub use drv_fpga_app_api::power_rail::*;

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
    fn state(&self) -> Result<PowerRailState, FpgaError> {
        self.fpga.read(Addr::FRONT_IO_STATE)
    }

    #[inline]
    pub fn status(&self) -> Result<PowerRailStatus, FpgaError> {
        PowerRailStatus::try_from(self.state()?)
    }

    pub fn set_enable(&self, enable: bool) -> Result<(), FpgaError> {
        let op = if enable {
            WriteOp::BitSet
        } else {
            WriteOp::BitClear
        };

        self.fpga
            .write(op, Addr::FRONT_IO_STATE, Reg::FRONT_IO_STATE::ENABLE)
    }
}
