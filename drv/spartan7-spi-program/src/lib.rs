// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Code for programming a Spartan 7 FPGA using separate GPIO and SPI servers.
//!
//! **Note:** this code makes an environmental assumption that you are not using
//! the task timer for anything else, because it uses it to implement delays. If
//! your task only uses `hl::sleep` and friends, you're safe.
//!
//! This module provides reset and bitstream load support for Spartan 7 FPGAs
//! connected over SPI. To use this module,
//!
//! 1. Create a `Config` struct filled out with your wiring details.
//! 2. Call `begin_bitstream_load` once.
//! 3. Call `continue_bitstream_load` as many times as you need to, passing
//!    chunks of data each time.
//! 4. Call `finish_bitstream_load` once to complete the process and check the
//!    result.
//!
//! If any of the operations fail, the intention is that you restart the process
//! from `begin_bitstream_load` -- it should handle the reset and clean up from
//! the earlier failure. However, this is only _somewhat_ tested.

#![no_std]

use drv_spi_api::{self as spi_api, SpiDevice, SpiServer};
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use userlib::hl;

/// Wiring configuration for the Spartan7 FPGA.
pub struct Config {
    /// Pin set where PROGRAM_L goes -- should only have one bit set.
    pub program_l: sys_api::PinSet,
    /// Pin set where INIT_L goes -- should only have one bit set.
    pub init_l: sys_api::PinSet,
    /// Pin set where CONFIG_DONE goes -- should only have one bit set.
    pub config_done: sys_api::PinSet,
    /// Pin set for user logic reset -- should only have one bit set.
    pub user_reset_l: sys_api::PinSet,
}

/// Things that we can _notice_ going wrong when programming -- the FPGA doesn't
/// actually give us a lot of feedback.
#[derive(counters::Count, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Spartan7Error {
    /// We attempted to put the chip into programming mode, but its INIT_L pin
    /// did not go high to confirm.
    ChipNotListening,
    /// We thought we loaded the entire bitstream, but the CONFIG_DONE pin did
    /// not go high. This may be a sign that you're sending a bitstream for a
    /// smaller FPGA.
    ConfigDidNotComplete,
    /// Communications over SPI failed while loading the bitstream
    SpiLoadWriteFailed(#[count(children)] spi_api::SpiError),
    /// Communications over SPI failed while writing bonus clocks
    SpiBonusClocksFailed(#[count(children)] spi_api::SpiError),
}

/// Bitstream for a Spartan-7 FPGA
pub struct BitstreamLoader<'a, S> {
    spi: &'a SpiDevice<S>,
    sys: &'a Sys,
    config: &'a Config,
}

impl<'a, S: SpiServer> BitstreamLoader<'a, S> {
    /// Sends messages to `sys` to configure the pins
    fn configure_pins(&self) {
        // Ensure our pins all start out in a reasonable state.

        // Configure the FPGA_INIT_L and FPGA_CONFIG_DONE lines as inputs
        self.sys
            .gpio_configure_input(self.config.init_l, sys_api::Pull::None);
        self.sys
            .gpio_configure_input(self.config.config_done, sys_api::Pull::None);

        // Configure FPGA_LOGIC_RESET_L as an output and make sure it's low
        self.sys.gpio_reset(self.config.user_reset_l);
        self.sys.gpio_configure_output(
            self.config.user_reset_l,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        // To allow for the possibility that we are restarting, rather than
        // starting, we take care during early sequencing to _not turn anything
        // off,_ only on. This means if it was _already_ on, the outputs should
        // not glitch.

        // To program the FPGA, we're using "slave serial" mode.
        //
        // See "7 Series FPGAs Configuration", UG470 (v1.17) for details,
        // as well as "Using a Microprocessor to Configure Xilinx 7 Series FPGAs
        // via Slave Serial or Slave SelectMAP Mode Application Note" (XAPP583)

        // Configure the PROGRAM_B line to the FPGA
        self.sys.gpio_set(self.config.program_l);
        self.sys.gpio_configure_output(
            self.config.program_l,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
    }

    /// Optionally configures pins, then puts the FPGA into programming mode
    ///
    /// If programming fails, you can call this again to restart.
    pub fn begin_bitstream_load(
        sys: &'a Sys,
        config: &'a Config,
        spi: &'a SpiDevice<S>,
        configure_pins: bool,
    ) -> Result<Self, Spartan7Error> {
        let out = Self { sys, config, spi };
        if configure_pins {
            out.configure_pins();
        }

        // Pulse PROGRAM_B low for 1 ms to reset the bitstream
        // (T_PROGRAM is 250 ns min, so this is fine)
        // https://docs.amd.com/r/en-US/ds189-spartan-7-data-sheet/XADC-Specifications
        sys.gpio_reset(config.program_l);
        hl::sleep_for(1);
        sys.gpio_set(config.program_l);

        // Tpl is 5 ms, let's give it 10ms to be conservative
        hl::sleep_for(10);

        // Check that init has gone high
        if sys.gpio_read(config.init_l) != 0 {
            Ok(out)
        } else {
            Err(Spartan7Error::ChipNotListening)
        }
    }

    /// Sends a chunk of data from the FPGA bitstream.
    ///
    /// We send data in chunks because the FPGA bitstreams are relatively large,
    /// and our RAM is relatively small. Chunk boundaries can fall anywhere in
    /// the bitstream, and chunks can vary in size if you need them to.
    ///
    /// Note that there is a 64kiB limitation in the current SPI controller, so,
    /// if you hit that you will get a `SpiError` back.
    pub fn continue_bitstream_load(
        &self,
        data: &[u8],
    ) -> Result<(), Spartan7Error> {
        // Loading the remainder of the bitstream is a simple matter of...
        self.spi
            .write(data)
            .map_err(Spartan7Error::SpiLoadWriteFailed)?;
        Ok(())
    }

    /// Wraps up bitstream loading and checks the `CONFIG_DONE` signal to see if
    /// it worked.
    pub fn finish_bitstream_load(self) -> Result<(), Spartan7Error> {
        // Wait for the FPGA to pull DONE high
        const DELAY_MS: u64 = 2;
        const TIMEOUT_MS: u64 = 250;
        let mut wait_time_ms = 0;
        while self.sys.gpio_read(self.config.config_done) == 0 {
            hl::sleep_for(DELAY_MS);
            wait_time_ms += DELAY_MS;
            if wait_time_ms > TIMEOUT_MS {
                return Err(Spartan7Error::ConfigDidNotComplete);
            }
        }

        // Send 64 bonus clocks to complete the startup sequence (see "Clocking
        // to End of Startup" in UG470).
        self.spi
            .write(&[0u8; 8])
            .map_err(Spartan7Error::SpiBonusClocksFailed)?;

        // Enable the user design
        self.sys.gpio_set(self.config.user_reset_l);

        Ok(())
    }
}
