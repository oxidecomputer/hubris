// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Crate implementing FPGA drivers for use with the FPGA server.

#![no_std]

use drv_fpga_api::{BitstreamType, DeviceState, FpgaError};

pub mod ecp5;
pub mod ecp5_spi;
pub mod ecp5_spi_mux_pca9538;

/// Trait to be implemented by FPGA device drivers in order to be exposed using
/// the FPGA server. This trait allows managing the FPGA device itself as well
/// provides reset control for the user_design implemented by the bitstream.
pub trait Fpga<'a> {
    type Bitstream: 'a + FpgaBitstream + Drop;

    /// Determine if the device is enabled (i.e. not in reset).
    fn device_enabled(&self) -> Result<bool, FpgaError>;

    /// Set whether or not the device is enabled.
    fn set_device_enabled(&self, enabled: bool) -> Result<(), FpgaError>;

    /// Reset the device, allowing it to load a bitstream.
    fn reset_device(&self) -> Result<(), FpgaError>;

    /// Return the current device state.
    fn device_state(&self) -> Result<DeviceState, FpgaError>;

    /// Return the device ID, if any.
    fn device_id(&self) -> Result<u32, FpgaError>;

    /// Start the process of loading a bitstream. The device being in a state
    /// where a bitstream can be loaded is a precondition for this method to
    /// execute correctly.
    fn start_bitstream_load(&'a self) -> Result<Self::Bitstream, FpgaError>;
}

pub trait FpgaBitstream {
    /// Load the next chunk of the bitstream.
    fn continue_load(&mut self, data: &[u8]) -> Result<(), FpgaError>;

    /// Finish loading the bitstream, allowing the device to transition to
    /// user mode.
    fn finish_load(&mut self) -> Result<(), FpgaError>;
}

pub trait FpgaUserDesign {
    /// Determine if the user design is enabled (i.e. not in reset).
    fn user_design_enabled(&self) -> Result<bool, FpgaError>;

    /// Set whether or not the user design is enabled.
    fn set_user_design_enabled(&self, enabled: bool) -> Result<(), FpgaError>;

    /// Reset the user design.
    fn reset_user_design(&self) -> Result<(), FpgaError>;

    /// Read/write to the user design.
    fn user_design_read(&self, buf: &mut [u8]) -> Result<(), FpgaError>;
    fn user_design_write(&self, buf: &[u8]) -> Result<(), FpgaError>;

    /// Lock the user design for multiple uninterrupted operations.
    ///
    /// Note: the semantics of this are not well defined and need work.
    fn user_design_lock(&self) -> Result<(), FpgaError>;

    /// Release the lock on the user design held previously.
    fn user_design_release(&self) -> Result<(), FpgaError>;
}

pub enum BitstreamLoader<'a, D: Fpga<'a>> {
    Uncompressed(D::Bitstream, usize),
    Compressed(gnarle::Decompressor, D::Bitstream, usize),
}

pub fn start_bitstream_load<'a, D: Fpga<'a>>(
    device: &'a D,
    bitstream_type: BitstreamType,
) -> Result<BitstreamLoader<'a, D>, FpgaError> {
    Ok(match bitstream_type {
        BitstreamType::Uncompressed => {
            BitstreamLoader::Uncompressed(device.start_bitstream_load()?, 0)
        }
        BitstreamType::Compressed => BitstreamLoader::Compressed(
            gnarle::Decompressor::default(),
            device.start_bitstream_load()?,
            0,
        ),
    })
}

pub fn continue_bitstream_load<'a, 'b, D: Fpga<'a>>(
    bitstream_loader: &'b mut BitstreamLoader<'a, D>,
    chunk: &mut [u8],
) -> Result<(), FpgaError> {
    match bitstream_loader {
        BitstreamLoader::Uncompressed(bitstream, len) => {
            bitstream.continue_load(chunk)?;
            *len += chunk.len();
        }
        BitstreamLoader::Compressed(decompressor, bitstream, len) => {
            let mut decompress_buffer = [0; 512];

            while !chunk.is_empty() {
                let decompressed_chunk = gnarle::decompress(
                    decompressor,
                    &mut &*chunk,
                    &mut decompress_buffer,
                );

                // The compressor may have encountered a partial run at the
                // end of the `chunk`, in which case `decompressed_chunk`
                // will be empty since more data is needed before output is
                // generated.
                if !decompressed_chunk.is_empty() {
                    bitstream.continue_load(decompressed_chunk)?;
                    *len += decompressed_chunk.len();
                }
            }
        }
    }

    Ok(())
}

pub fn finish_bitstream_load<'a, 'b, D: Fpga<'a>>(
    bitstream_loader: &'b mut BitstreamLoader<'a, D>,
) -> Result<usize, FpgaError> {
    match bitstream_loader {
        BitstreamLoader::Uncompressed(bitstream, len) => {
            bitstream.finish_load()?;
            Ok(*len)
        }
        BitstreamLoader::Compressed(_, bitstream, len) => {
            bitstream.finish_load()?;
            Ok(*len)
        }
    }
}
