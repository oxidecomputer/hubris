// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_fpga_api::{
    BitstreamType, DeviceState, Fpga, FpgaError, FpgaUserDesign,
};
use userlib::hl::sleep_for;

static COMPRESSED_BITSTREAM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/ecp5.bin.rle"));

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

    /// Poll the device state of the FPGA to determine if it is ready to receive
    /// a bitstream, resetting the device if needed.
    pub fn await_fpga_ready(
        &mut self,
        sleep_ticks: u64,
    ) -> Result<DeviceState, FpgaError> {
        let mut state = self.fpga.state()?;

        while match state {
            DeviceState::AwaitingBitstream | DeviceState::RunningUserDesign => {
                false
            }
            _ => true,
        } {
            self.fpga.reset()?;
            sleep_for(sleep_ticks);
            state = self.fpga.state()?;
        }

        Ok(state)
    }

    /// Load the mainboard controller bitstream.
    pub fn load_bitstream(&mut self) -> Result<(), FpgaError> {
        let mut bitstream =
            self.fpga.start_bitstream_load(BitstreamType::Compressed)?;

        for chunk in COMPRESSED_BITSTREAM[..].chunks(128) {
            bitstream.continue_load(chunk)?;
        }

        bitstream.finish_load()
    }

    /// Reads the IDENT0:3 registers as a big-endian 32-bit integer.
    pub fn ident(&self) -> Result<u32, FpgaError> {
        Ok(u32::from_be(self.user_design.read(Addr::ID0)?))
    }

    /// Check for a valid identifier
    pub fn ident_valid(&self, ident: u32) -> bool {
        ident == Self::EXPECTED_IDENT
    }
}
