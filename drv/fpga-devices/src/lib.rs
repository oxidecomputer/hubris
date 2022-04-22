// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Crate implementing FPGA drivers for use with the FPGA server.

#![no_std]

use drv_fpga_api::{DeviceState, FpgaError};

pub mod ecp5;
pub mod ecp5_spi;

/// Trait to be implemented by FPGA device drivers in order to allow to be
/// exposed using the FPGA server. This trait allows managing the FPGA device
/// itself as well provides reset control for the application implemented
/// through the bitstream.
pub trait Fpga {
    /// Determine if the device is enabled (i.e. not in reset).
    fn device_enabled(&self) -> Result<bool, FpgaError>;
    /// Set whether or not the device is enabled.
    fn set_device_enable(&mut self, enabled: bool) -> Result<(), FpgaError>;
    /// Reset the device, allowing it to load a bitstream.
    fn reset_device(&mut self, ticks: u64) -> Result<(), FpgaError>;
    /// Return the current device state.
    fn device_state(&self) -> Result<DeviceState, FpgaError>;
    /// Return the device ID, if any.
    fn device_id(&self) -> Result<u32, FpgaError>;

    /// Start the process of loading a bitstream. The device being in a state
    /// where a bitstream can be loaded is a precondition for this method to
    /// execute correctly.
    fn start_bitstream_load(&mut self) -> Result<(), FpgaError>;
    /// Load the next chunk of the bitstream.
    fn continue_bitstream_load(&mut self, data: &[u8])
        -> Result<(), FpgaError>;
    /// Finish loading the bitstream, allowing the device to transition to
    /// application mode.
    fn finish_bitstream_load(
        &mut self,
        application_reset_ticks: u64,
    ) -> Result<(), FpgaError>;

    /// Determine if the bitstream application is enabled (i.e. not in reset).
    fn application_enabled(&self) -> Result<bool, FpgaError>;
    /// Set whether or not the bitstream application is enabled.
    fn set_application_enable(
        &mut self,
        enabled: bool,
    ) -> Result<(), FpgaError>;
    /// Reset the bitstream application.
    fn reset_application(&mut self, ticks: u64) -> Result<(), FpgaError>;
}
