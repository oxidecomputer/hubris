// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, SIDECAR_IO_BITSTREAM_CHECKSUM};
use drv_fpga_api::*;
use userlib::UnwrapLite;

pub struct FrontIOController {
    fpga: Fpga,
    pub user_design: FpgaUserDesign,
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
    pub fn ready(&self) -> Result<bool, FpgaError> {
        Ok(self.fpga.state()? == DeviceState::RunningUserDesign)
    }

    #[inline]
    pub fn await_fpga_ready(
        &mut self,
        sleep_ticks: u64,
    ) -> Result<DeviceState, FpgaError> {
        await_fpga_ready(&mut self.fpga, sleep_ticks)
    }

    /// Load the front io board controller bitstream, pulling it from the
    /// attached auxiliary flash.
    #[inline]
    pub fn load_bitstream(
        &mut self,
        auxflash: userlib::TaskId,
    ) -> Result<(), FpgaError> {
        let mut auxflash = drv_auxflash_api::AuxFlash::from(auxflash);
        let blob = auxflash
            .get_blob_by_tag(*b"QSFP")
            .map_err(|_| FpgaError::AuxMissingBlob)?;
        drv_fpga_api::load_bitstream_from_auxflash(
            &mut self.fpga,
            &mut auxflash,
            blob,
            BitstreamType::Compressed,
            SIDECAR_IO_BITSTREAM_CHECKSUM,
        )
    }

    /// Check for a valid identifier
    pub fn ident_valid(&self) -> Result<(u32, bool), FpgaError> {
        let ident = u32::from_be(self.user_design.read(Addr::ID0)?);
        Ok((ident, ident == Self::EXPECTED_IDENT))
    }

    /// Loads the checksum scratchpad register and checks whether it matches
    /// our expected checksum.
    ///
    /// This allows us to detect cases where the Hubris image has been updated
    /// while the FPGA remained powered: if the FPGA bitstream in the new
    /// Hubris image has changed, the checksum will no longer match.
    pub fn checksum_valid(&self) -> Result<([u8; 4], bool), FpgaError> {
        let checksum = self.user_design.read(Addr::CHECKSUM_SCRATCHPAD0)?;
        Ok((checksum, checksum == Self::short_checksum()))
    }

    /// Writes the checksum scratchpad register to our expected checksum.
    ///
    /// In concert with `checksum_valid`, this lets us detect cases where the
    /// Hubris image has rebooted and the FPGA image should be updated.
    pub fn write_checksum(&self) -> Result<(), FpgaError> {
        self.user_design.write(
            WriteOp::Write,
            Addr::CHECKSUM_SCRATCHPAD0,
            Self::short_checksum(),
        )
    }

    /// Returns the expected (short) checksum, which simply a prefix of the full
    /// SHA3-256 hash of the bitstream.
    pub fn short_checksum() -> [u8; 4] {
        SIDECAR_IO_BITSTREAM_CHECKSUM[..4].try_into().unwrap_lite()
    }
}
