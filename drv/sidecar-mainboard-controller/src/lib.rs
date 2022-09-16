// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_fpga_api::*;

include!(concat!(env!("OUT_DIR"), "/sidecar_mainboard_controller.rs"));

pub mod tofino2;

pub struct MainboardController {
    fpga: Fpga,
    user_design: FpgaUserDesign,
}

impl MainboardController {
    pub const DEVICE_INDEX: u8 = 0;
    pub const EXPECTED_IDENT: u32 = 0x1DE_AA55;

    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            fpga: Fpga::new(task_id, Self::DEVICE_INDEX),
            user_design: FpgaUserDesign::new(task_id, Self::DEVICE_INDEX),
        }
    }

    pub fn reset(&mut self) -> Result<(), FpgaError> {
        self.fpga.reset()
    }

    /// Poll the device state of the FPGA to determine if it is ready to receive
    /// a bitstream, resetting the device if needed.
    pub fn await_fpga_ready(
        &mut self,
        sleep_ticks: u64,
    ) -> Result<DeviceState, FpgaError> {
        await_fpga_ready(&mut self.fpga, sleep_ticks)
    }

    /// Load the mainboard controller bitstream.
    pub fn load_bitstream(
        &mut self,
        auxflash: userlib::TaskId,
    ) -> Result<(), FpgaError> {
        let mut auxflash = drv_auxflash_api::AuxFlash::from(auxflash);
        let blob = auxflash
            .get_blob_by_tag(*b"FPGA")
            .map_err(|_| FpgaError::AuxMissingBlob)?;
        drv_fpga_api::load_bitstream_from_auxflash(
            &mut self.fpga,
            &mut auxflash,
            blob,
            BitstreamType::Compressed,
            SIDECAR_MAINBOARD_BITSTREAM_CHECKSUM,
        )
    }

    /// Check for a valid peripheral identifier.
    pub fn ident_valid(&self) -> Result<(u32, bool), FpgaError> {
        let ident = u32::from_be(self.user_design.read(Addr::ID0)?);
        Ok((ident, ident == Self::EXPECTED_IDENT))
    }
}
