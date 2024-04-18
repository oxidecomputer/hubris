// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;
use drv_fpga_api::{DeviceState, FpgaError};
use drv_front_io_api::{controller::FrontIOController, phy_smi::PhySmi};
use drv_i2c_devices::{at24csw080::At24Csw080, Validate};

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
        let fruid = i2c_config::devices::at24csw080_front_io0(i2c_task)[0];
        At24Csw080::validate(&fruid).unwrap_or(false)
    }

    pub fn initialized(&self) -> bool {
        self.controllers.iter().all(|c| c.ready().unwrap_or(false))
    }

    pub fn init(&mut self) -> Result<bool, FpgaError> {
        let mut controllers_ready = true;

        for (i, controller) in self.controllers.iter_mut().enumerate() {
            let state = controller.await_fpga_ready(25)?;
            let mut ident;
            let mut ident_valid = false;
            let mut checksum;
            let mut checksum_valid = false;

            if state == DeviceState::RunningUserDesign {
                (ident, ident_valid) = controller.ident_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerIdent {
                    fpga_id: i,
                    ident
                });

                (checksum, checksum_valid) = controller.checksum_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerChecksum {
                    fpga_id: i,
                    checksum,
                    expected: FrontIOController::short_checksum(),
                });

                if !ident_valid || !checksum_valid {
                    // Attempt to correct the invalid IDENT by reloading the
                    // bitstream.
                    controller.fpga_reset()?;
                }
            }

            if ident_valid && checksum_valid {
                ringbuf_entry!(Trace::SkipLoadingFrontIOControllerBitstream {
                    fpga_id: i
                });
            } else {
                ringbuf_entry!(Trace::LoadingFrontIOControllerBitstream {
                    fpga_id: i
                });

                if let Err(e) = controller.load_bitstream(self.auxflash_task) {
                    ringbuf_entry!(Trace::FpgaBitstreamError(u32::from(e)));
                    return Err(e);
                }

                (ident, ident_valid) = controller.ident_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerIdent {
                    fpga_id: i,
                    ident
                });

                controller.write_checksum()?;
                (checksum, checksum_valid) = controller.checksum_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerChecksum {
                    fpga_id: i,
                    checksum,
                    expected: FrontIOController::short_checksum(),
                });
            }

            controllers_ready &= ident_valid & checksum_valid;
        }

        Ok(controllers_ready)
    }
}
