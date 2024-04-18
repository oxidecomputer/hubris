// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_fpga_api::*;

include!(concat!(env!("OUT_DIR"), "/sidecar_mainboard_controller.rs"));

pub mod fan_modules;
pub mod ignition;
pub mod tofino2;

pub struct MainboardController {
    fpga: Fpga,
    user_design: FpgaUserDesign,
}

impl MainboardController {
    pub const DEVICE_INDEX: u8 = 0;
    pub const EXPECTED_ID: u32 = 0x01de_5bae;

    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            fpga: Fpga::new(task_id, Self::DEVICE_INDEX),
            user_design: FpgaUserDesign::new(task_id, Self::DEVICE_INDEX),
        }
    }

    pub fn reset(&mut self) -> Result<(), FpgaError> {
        self.fpga.reset()
    }

    pub fn ready(&self) -> Result<bool, FpgaError> {
        self.fpga
            .state()
            .map(|s| s == DeviceState::RunningUserDesign)
    }

    /// Poll the device state of the FPGA to determine if it is ready to receive
    /// a bitstream, resetting the device if needed.
    pub fn await_fpga_ready(
        &mut self,
        sleep_ticks: u64,
    ) -> Result<DeviceState, FpgaError> {
        await_fpga_ready(&mut self.fpga, sleep_ticks)
    }

    /// Read the design ident.
    pub fn read_ident(&self) -> Result<FpgaUserDesignIdent, FpgaError> {
        self.user_design.read(Addr::ID0)
    }

    /// Load the mainboard controller bitstream.
    #[cfg(feature = "bitstream")]
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

    /// Returns the expected (short) checksum, which simply a prefix of the full
    /// SHA3-256 hash of the bitstream.
    #[cfg(feature = "bitstream")]
    pub fn short_bitstream_checksum() -> u32 {
        u32::from_le_bytes(
            SIDECAR_MAINBOARD_BITSTREAM_CHECKSUM[..4]
                .try_into()
                .unwrap(),
        )
    }

    /// Set the checksum write-once registers to the expected checksum.
    ///
    /// In concert with `short_bitstream_checksum_valid`, this will detect when
    /// the bitstream of an already running mainboard controller does
    /// (potentially) not match the APIs used to build Hubris.
    #[cfg(feature = "bitstream")]
    pub fn set_short_bitstream_checksum(&self) -> Result<(), FpgaError> {
        self.user_design.write(
            WriteOp::Write,
            Addr::CS0,
            Self::short_bitstream_checksum().to_be(),
        )
    }

    /// Check whether the Ident checksum matches the short bitstream checksum.
    ///
    /// This allows us to detect cases where the Hubris image has been updated
    /// while the FPGA remained powered: if the checksum of the FPGA bitstream
    /// in the new Hubris image has changed it will no longer match the Ident.
    #[cfg(feature = "bitstream")]
    pub fn short_bitstream_checksum_valid(
        &self,
        ident: &FpgaUserDesignIdent,
    ) -> bool {
        ident.checksum.get() == Self::short_bitstream_checksum()
    }
}
