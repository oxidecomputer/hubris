//! Code for programming an iCE40 FPGA using separate GPIO and SPI servers.
//!
//! **Note:** this code makes an environmental assumption that you are not using
//! the task timer for anything else, because it uses it to implement delays. If
//! your task only uses `hl::sleep` and friends, you're safe.
//!
//! This module provides reset and bitstream load support for iCE40 FPGAs
//! connected over SPI. To use this module,
//!
//! 1. Create a `Config` struct filled out with your wiring details.
//! 2. Call `configure_pins`, or arrange to configure the CRESET/CDONE pins
//!    yourself.
//! 3. Call `begin_bitstream_load` once.
//! 4. Call `continue_bitstream_load` as many times as you need to, passing
//!    chunks of data each time.
//! 5. Call `finish_bitstream_load` once to complete the process and check the
//!    result.
//!
//! If any of the operations fail, the intention is that you restart the process
//! from `begin_bitstream_load` -- it should handle the reset and clean up from
//! the earlier failure. However, this is only _somewhat_ tested.

#![no_std]

use drv_spi_api::{self as spi_api, SpiDevice};
use drv_stm32h7_gpio_api::{self as gpio_api, Gpio};
use userlib::hl;

/// Wiring configuration for the iCE40 FPGA.
pub struct Config {
    /// Port where CRESETB goes.
    pub creset_port: gpio_api::Port,
    /// Pin mask where CRESETB goes -- should only have one bit set.
    pub creset_pin_mask: u16,
    /// Port where CDONE goes.
    pub cdone_port: gpio_api::Port,
    /// Pin mask where CDONE goes -- should only have one bit set.
    pub cdone_pin_mask: u16,
}

/// Things that we can _notice_ going wrong when programming -- the FPGA doesn't
/// actually give us a lot of feedback.
pub enum Ice40Error {
    /// We attempted to put the chip into programming mode, but its CDONE pin
    /// did not go low to confirm.
    ChipNotListening,
    /// We thought we loaded the entire bitstream, but the CDONE pin did not go
    /// high. This may be a sign that you're sending a bitstream for a smaller
    /// FPGA.
    ConfigDidNotComplete,
    /// Communications over SPI failed (reason attached).
    Spi(spi_api::SpiError),
}

impl From<spi_api::SpiError> for Ice40Error {
    fn from(x: spi_api::SpiError) -> Self {
        Self::Spi(x)
    }
}

/// Sends messages to `gpio` to configure the pins described in `Config` so that
/// you don't have to.
pub fn configure_pins(gpio: &Gpio, config: &Config) {
    // Ensure our pins all start out in a reasonable state.
    // Note that the SPI server manages CS for us. We want RESET to be
    // not-asserted but ready to assert. This ensures that we don't glitch RESET
    // low (active!) when we make it an output below.
    gpio.set_reset(
        config.creset_port,
        config.creset_pin_mask, // set = inactive
        0,
    )
    .unwrap();
    // Make RESET an output.
    gpio.configure(
        config.creset_port,
        config.creset_pin_mask,
        gpio_api::Mode::Output,
        gpio_api::OutputType::OpenDrain,
        gpio_api::Speed::High,
        gpio_api::Pull::None,     // external resistor on net
        gpio_api::Alternate::AF0, // doesn't matter
    )
    .unwrap();
    // And finally we need CDONE to be an input.
    gpio.configure(
        config.cdone_port,
        config.cdone_pin_mask,
        gpio_api::Mode::Input,
        gpio_api::OutputType::OpenDrain, // don't care
        gpio_api::Speed::High,           // don't care
        gpio_api::Pull::None,            // don't care
        gpio_api::Alternate::AF0,        // don't care
    )
    .unwrap();
}

/// Runs the iCE40 through its programming reset sequence and puts it into SPI
/// target mode.
///
/// On success, this has _locked_ the `spi` controller for exclusive access.
/// This means it would be polite to proceed with programming promptly.
///
/// If programming fails, you can call this again to restart. If you want to
/// abort programming after a failure, use `spi.release()`.
pub fn begin_bitstream_load(
    spi: &SpiDevice,
    gpio: &Gpio,
    config: &Config,
) -> Result<(), Ice40Error> {
    // We directly control two iCE40-specific signals, CRESET and CDONE.
    // Configure them.

    // We're going to be doing a series of odd things involving SPI CS, CRESET,
    // and CDONE. This requires us to have exclusive control over the SPI bus.

    // Assert reset (active low).
    gpio.set_reset(config.creset_port, 0, config.creset_pin_mask)
        .unwrap();

    // Lock SPI controller and assert CS.
    spi.lock(spi_api::CsState::Asserted)?;

    // Minimum duration of reset pulse is 200ns. One of our 1ms ticks will be
    // fine.
    hl::sleep_for(1);

    // Deassert reset (active low).
    gpio.set_reset(config.creset_port, config.creset_pin_mask, 0)
        .unwrap();

    // Minimum time to stabilize here is either 300us or 800us, depending on
    // which Lattice doc you're reading. Give it 2ms to be sure.
    hl::sleep_for(2);

    // At this point, the iCE40 is _supposed_ to be chilling in programming mode
    // listening for a bitstream. If this is the case it will be asserting
    // (holding low) CDONE. Let's check!
    if gpio.read_input(config.cdone_port).unwrap() & config.cdone_pin_mask != 0
    {
        // Welp, that sure didn't work.
        return Err(Ice40Error::ChipNotListening);
    }

    // At this point, the icoprog tooling performs 8 cycles of clock-twiddling.
    // I can't find any mention of this in the Lattice docs, but it also doesn't
    // appear to _hurt_ because the real bitstream starts with a recognizeable
    // pattern. And so,
    spi.write(&[0xFF])?;
    Ok(())
}

/// Sends a chunk of data from the FPGA bitstream. This should follow a
/// (successful!) call to `begin_bitstream_load`.
///
/// We send data in chunks because the FPGA bitstreams are relatively large, and
/// our RAM is relatively small. Chunk boundaries can fall anywhere in the
/// bitstream, and chunks can vary in size if you need them to. This has been
/// tested with chunks down to 1 byte and up to 1024 and seems to work.
///
/// Note that there is a 64kiB limitation in the current SPI controller, so,
/// if you hit that you will get a `SpiError` back.
pub fn continue_bitstream_load(
    spi: &SpiDevice,
    data: &[u8],
) -> Result<(), spi_api::SpiError> {
    // Loading the remainder of the bitstream is a simple matter of...
    spi.write(data)?;
    Ok(())
}

/// Wraps up bitstream loading and checks the CDONE signal to see if it worked.
///
/// This also unlocks the SPI controller.
pub fn finish_bitstream_load(
    spi: &SpiDevice,
    gpio: &Gpio,
    config: &Config,
) -> Result<(), Ice40Error> {
    // If we've sent the bitstream successfully, we expect the iCE40 to release
    // CDONE. This is supposed to happen fairly quickly. Give it a bit and
    // check.
    hl::sleep_for(1);
    if gpio.read_input(config.cdone_port).unwrap() & config.cdone_pin_mask == 0
    {
        // aw shucks
        return Err(Ice40Error::ConfigDidNotComplete);
    }

    // After receiving the bitstream, the iCE40 wants 49 or more clock edges.
    // Because 48 would be too easy. So, we'll send 56.
    spi.write(&[0xFF; 56 / 8])?;

    // And, at this point, we can release SPI.
    spi.release().unwrap();

    Ok(())
}
