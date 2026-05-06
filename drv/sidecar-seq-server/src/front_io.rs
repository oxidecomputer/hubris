// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;
use drv_front_io_api::controller::FrontIOController;
use drv_front_io_api::phy_smi::PhySmi;
use drv_i2c_devices::{Validate, at24csw080::At24Csw080};

#[allow(dead_code)]
pub(crate) struct FrontIOBoard {
    pub controllers: [FrontIOController; 2],
    fpga_task: userlib::TaskId,
    auxflash_task: userlib::TaskId,
}

impl FrontIOBoard {
    pub fn new(
        fpga_task: userlib::TaskId,
        auxflash_task: userlib::TaskId,
    ) -> Self {
        Self {
            controllers: [
                FrontIOController::new(fpga_task, 0),
                FrontIOController::new(fpga_task, 1),
            ],
            fpga_task,
            auxflash_task,
        }
    }

    pub fn phy(&self) -> PhySmi {
        PhySmi::new(self.fpga_task)
    }

    pub fn present(i2c_task: userlib::TaskId) -> bool {
        let fruid = i2c_config::devices::at24csw080_front_io(i2c_task)[0];
        At24Csw080::validate(&fruid).unwrap_or(false)
    }

    pub fn initialized(&self) -> bool {
        self.controllers.iter().all(|c| c.ready().unwrap_or(false))
    }
}
