// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;
use drv_i2c_devices::{at24csw080::At24Csw080, Validate};
use drv_sidecar_front_io::{
    controller::FrontIOController, phy_smi::PhySmi,
    SIDECAR_IO_BITSTREAM_CHECKSUM,
};

#[allow(dead_code)]
pub(crate) struct FrontIOBoard {
    pub fruid: I2cDevice,
    pub controllers: [FrontIOController; 2],
    pub state_reset: bool,
    fpga_task: userlib::TaskId,
}

impl FrontIOBoard {
    pub fn new(fpga_task: userlib::TaskId, i2c_task: userlib::TaskId) -> Self {
        Self {
            fruid: i2c_config::devices::at24csw080_front_io(i2c_task)[0],
            controllers: [
                FrontIOController::new(fpga_task, 0),
                FrontIOController::new(fpga_task, 1),
            ],
            state_reset: false,
            fpga_task,
        }
    }

    pub fn phy_smi(&self) -> PhySmi {
        PhySmi::new(self.fpga_task)
    }

    pub fn present(&self) -> bool {
        At24Csw080::validate(&self.fruid).unwrap_or(false)
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
                    expected: SIDECAR_IO_BITSTREAM_CHECKSUM,
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

                if let Err(e) = controller.load_bitstream() {
                    ringbuf_entry!(Trace::FpgaBitstreamError(
                        u32::try_from(e).unwrap()
                    ));
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
                    expected: SIDECAR_IO_BITSTREAM_CHECKSUM,
                });
            }

            controllers_ready &= ident_valid & checksum_valid;
        }

        Ok(controllers_ready)
    }
}
