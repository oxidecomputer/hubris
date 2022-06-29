// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Addr;
use drv_fpga_api::*;

static COMPRESSED_BITSTREAM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/ecp5.bin.rle"));

pub struct FrontIOController {
    fpga: Fpga,
    user_design: FpgaUserDesign,
}

impl FrontIOController {
    pub const EXPECTED_IDENT: u32 = 0x1DE_AA55;

    pub fn new(task_id: userlib::TaskId, device_index: u8) -> Self {
        Self {
            fpga: Fpga::new(task_id, device_index),
            user_design: FpgaUserDesign::new(task_id, device_index),
        }
    }

    #[inline]
    pub fn fpga_reset(&mut self) -> Result<(), FpgaError> {
        self.fpga.reset()
    }

    #[inline]
    pub fn await_fpga_ready(
        &mut self,
        sleep_ticks: u64,
    ) -> Result<DeviceState, FpgaError> {
        await_fpga_ready(&mut self.fpga, sleep_ticks)
    }

    /// Load the front io board controller bitstream.
    #[inline]
    pub fn load_bitstream(&mut self) -> Result<(), FpgaError> {
        drv_fpga_api::load_bitstream(
            &mut self.fpga,
            &COMPRESSED_BITSTREAM[..],
            BitstreamType::Compressed,
            128,
        )
    }

    /// Check for a valid identifier
    pub fn ident_valid(&self) -> Result<(u32, bool), FpgaError> {
        let ident = u32::from_be(self.user_design.read(Addr::ID0)?);
        Ok((ident, ident == Self::EXPECTED_IDENT))
    }
}
