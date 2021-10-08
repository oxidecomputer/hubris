//! Server for managing the Gimlet sequencer FPGA.

#![no_std]
#![no_main]

use userlib::*;

use drv_ice40_spi_program as ice40;
use drv_spi_api as spi_api;
use drv_stm32h7_gpio_api as gpio_api;

#[export_name = "main"]
fn main() -> ! {
    // At any restart of this driver, we want to set the communication interface
    // back to the expected state, and then reach out to the sequencer to see if
    // it's (1) alive and (2) the right version.
    let spi = spi_api::Spi::from(get_task_id(SPI));
    let gpio = gpio_api::Gpio::from(get_task_id(GPIO));

    // Ensure our pins all start out in a reasonable state.
    // Note that the SPI server manages CS for us. We want RESET to be
    // not-asserted but ready to assert.
    ice40::configure_pins(&gpio, &ICE40_CONFIG);

    if let Some((port, pin_mask)) = GLOBAL_RESET {
        // Also configure our reset net. We're assuming push-pull because all
        // our boards with reset nets are lacking pullups right now.
        gpio.configure(
            port,
            pin_mask,
            gpio_api::Mode::Output,
            gpio_api::OutputType::PushPull,
            gpio_api::Speed::High,
            gpio_api::Pull::None,
            gpio_api::Alternate::AF0, // doesn't matter
        )
        .unwrap();
    }

    // TODO except for now we're going to skip the version check and
    // unconditionally reprogram it because yolo.
    let reprogram = true;

    // We only want to reset and reprogram the FPGA when absolutely required.
    if reprogram {
        if let Some((port, pin_mask)) = GLOBAL_RESET {
            // Assert the design reset signal (not the same as the FPGA
            // programming logic reset signal).
            gpio.set_reset(
                port, 0, pin_mask, // active low
            )
            .unwrap();
        }

        // Reprogramming will continue until morale improves.
        loop {
            match reprogram_fpga(&spi, &gpio, &ICE40_CONFIG) {
                Ok(()) => {
                    // yay
                    if let Some((port, pin_mask)) = GLOBAL_RESET {
                        // Deassert design reset signal. We set the pin, as it's
                        // active low.
                        gpio.set_reset(port, pin_mask, 0).unwrap();
                    }
                    break;
                }
                Err(_) => {
                    // Try and put state back to something reasonable.
                    // We don't know if we're still locked, so ignore the complaint
                    // if we're not.
                    let _ = spi.release();
                    // We're gonna try again.
                }
            }
        }
    }

    // FPGA should now be programmed.
    loop {
        // TODO this is where, like, sequencer stuff goes
        hl::sleep_for(10);
    }
}

fn reprogram_fpga(
    spi: &spi_api::Spi,
    gpio: &gpio_api::Gpio,
    config: &ice40::Config,
) -> Result<(), ice40::Ice40Error> {
    ice40::begin_bitstream_load(&spi, &gpio, &config)?;

    // We've got the bitstream in Flash, so we can technically just send it in
    // one transaction, but we'll want chunking later -- so let's make sure
    // chunking works.
    const CHUNK_SIZE: usize = 512;
    for chunk in BITSTREAM.chunks(CHUNK_SIZE) {
        ice40::continue_bitstream_load(&spi, chunk)?;
    }

    ice40::finish_bitstream_load(&spi, &gpio, &config)
}

static BITSTREAM: &[u8] = include_bytes!("../fpga.bin");

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimletlet-2")] {
        declare_task!(GPIO, gpio_driver);
        declare_task!(SPI, spi_driver);

        const ICE40_CONFIG: ice40::Config = ice40::Config {
            creset_port: gpio_api::Port::B,
            creset_pin_mask: 1 << 10,
            cdone_port: gpio_api::Port::E,
            cdone_pin_mask: 1 << 15,
        };

        const GLOBAL_RESET: Option<(gpio_api::Port, u16)> = None;
    } else if #[cfg(target_board = "gimlet-1")] {
        declare_task!(GPIO, gpio_driver);
        declare_task!(SPI, spi4_driver);

        const ICE40_CONFIG: ice40::Config = ice40::Config {
            // CRESET net is SEQ_TO_SP_CRESET_L and hits PD5.
            creset_port: gpio_api::Port::D,
            creset_pin_mask: 1 << 5,
            // CDONE net is SEQ_TO_SP_CDONE_L and hits PB4.
            cdone_port: gpio_api::Port::B,
            cdone_pin_mask: 1 << 4,
        };

        const GLOBAL_RESET: Option<(gpio_api::Port, u16)> = Some((
            gpio_api::Port::A,
            1 << 6,
        ));
    } else {
        compiler_error!("unsupported target board");
    }
}
