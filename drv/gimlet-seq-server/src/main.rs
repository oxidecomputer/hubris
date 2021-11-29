// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Gimlet sequencing process.
//!
//!

#![no_std]
#![no_main]

use userlib::*;

use drv_ice40_spi_program as ice40;
use drv_spi_api as spi_api;
use drv_stm32h7_gpio_api as gpio_api;

task_slot!(GPIO, gpio_driver);
task_slot!(SPI, spi_driver);

#[export_name = "main"]
fn main() -> ! {
    let spi = spi_api::Spi::from(SPI.get_task_id());
    let gpio = gpio_api::Gpio::from(GPIO.get_task_id());

    // To allow for the possibility that we are restarting, rather than
    // starting, we take care during early sequencing to _not turn anything
    // off,_ only on. This means if it was _already_ on, the outputs should not
    // glitch.

    // Unconditionally set our power-good detects as inputs.
    //
    // This is the expected reset state, but, good to be sure.
    gpio.configure(
        PGS_PORT,
        PG_V1P2_MASK | PG_V3P3_MASK,
        gpio_api::Mode::Input,
        gpio_api::OutputType::PushPull, // doesn't matter
        gpio_api::Speed::High,
        PGS_PULL,
        gpio_api::Alternate::AF0, // doesn't matter
    )
    .unwrap();

    // Unconditionally set our sequencing-related GPIOs to outputs.
    //
    // If the processor has reset, these will start out low. Since neither rail
    // has external pullups, this puts the regulators into a well-defined "off"
    // state instead of leaving them floating, which is the state when A2 power
    // starts coming up.
    //
    // If it's just our driver that has reset, this will have no effect, and
    // will continue driving the lines at whatever level we left them in.
    gpio.configure(
        ENABLES_PORT,
        ENABLE_V1P2_MASK | ENABLE_V3P3_MASK,
        gpio_api::Mode::Output,
        gpio_api::OutputType::PushPull,
        gpio_api::Speed::High,
        gpio_api::Pull::None,
        gpio_api::Alternate::AF0, // doesn't matter
    )
    .unwrap();

    // To talk to the sequencer we need to configure its pins, obvs. Note that
    // the SPI and CS lines are separately managed by the SPI server; the ice40
    // crate handles the CRESETB and CDONE signals, and takes care not to
    // generate surprise resets.
    ice40::configure_pins(&gpio, &ICE40_CONFIG);

    // Force iCE40 CRESETB low before turning power on. This is nice because it
    // prevents the iCE40 from racing us and deciding it should try to load from
    // Flash. TODO: this may cause trouble with hot restarts, test.
    gpio.set_reset(ICE40_CONFIG.creset_port, 0, ICE40_CONFIG.creset_pin_mask)
        .unwrap();

    // Begin, or resume, the power supply sequencing process for the FPGA. We're
    // going to be reading back our enable line states to get the real state
    // being seen by the regulators, etc.

    // The V1P2 regulator comes up first. It may already be on from a past life
    // of ours. Ensuring that it's on by writing the pin is just as cheap as
    // sensing its current state, and less code than _conditionally_ writing the
    // pin, so:
    gpio.set_reset(ENABLES_PORT, ENABLE_V1P2_MASK, 0).unwrap();

    // We don't actually know how long ago the regulator turned on. Could have
    // been _just now_ (above) or may have already been on. We'll use the PG pin
    // to detect when it's stable. But -- the PG pin on the LT3072 is initially
    // high when you turn the regulator on, and then takes time to drop if
    // there's a problem. So, to ensure that there has been at least 1ms since
    // regulator-on, we will delay for 2.
    hl::sleep_for(2);

    // Now, monitor the PG pin.
    loop {
        // active high
        let pg = gpio.read_input(PGS_PORT).unwrap() & PG_V1P2_MASK != 0;
        if pg {
            break;
        }

        // Do _not_ burn CPU constantly polling, it's rude. We could also set up
        // pin-change interrupts but we only do this once per power on, so it
        // seems like a lot of work.
        hl::sleep_for(2);
    }

    // We believe V1P2 is good. Now, for V3P3! Set it active (high).
    gpio.set_reset(ENABLES_PORT, ENABLE_V3P3_MASK, 0).unwrap();

    // Delay to be sure.
    hl::sleep_for(2);

    // Now, monitor the PG pin.
    loop {
        // active high
        let pg = gpio.read_input(PGS_PORT).unwrap() & PG_V3P3_MASK != 0;
        if pg {
            break;
        }

        // Do _not_ burn CPU constantly polling, it's rude.
        hl::sleep_for(2);
    }

    // Now, V2P5 is chained off V3P3 and comes up on its own with no
    // synchronization. It takes about 500us in practice. We'll delay for 1ms,
    // plus give the iCE40 a good 10ms to come out of power-down.
    hl::sleep_for(1 + 10);

    // Sequencer FPGA power supply sequencing (meta-sequencing?) is complete.

    // Now, let's find out if we need to program the sequencer.

    if let Some(hacks) = FPGA_HACK_PINS {
        // Some boards require certain pins to be put in certain states before
        // we can perform SPI communication with the design (rather than the
        // programming port). If this is such a board, apply those changes:
        for &(port, pin_mask, is_high) in hacks {
            gpio.set_reset(
                port,
                if is_high { pin_mask } else { 0 },
                if is_high { 0 } else { pin_mask },
            )
            .unwrap();

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
    }

    if let Some((port, pin_mask)) = GLOBAL_RESET {
        // Also configure our design reset net -- the signal that resets the
        // logic _inside_ the FPGA instead of the FPGA itself. We're assuming
        // push-pull because all our boards with reset nets are lacking pullups
        // right now. It's active low, so, set up the pin before exposing the
        // output to ensure we don't glitch.
        gpio.set_reset(port, pin_mask, 0).unwrap();
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

    // If the sequencer is already loaded and operational, the design loaded
    // into it should be willing to talk to us over SPI, and should be able to
    // serve up a recognizable ident code.
    //
    // TODO except for now we're going to skip the version check and
    // unconditionally reprogram it because the SPI communication code ain't
    // written, and also yolo. Replace this with a check.
    let reprogram = true;

    // We only want to reset and reprogram the FPGA when absolutely required.
    if reprogram {
        if let Some((port, pin_mask)) = GLOBAL_RESET {
            // Assert the design reset signal (not the same as the FPGA
            // programming logic reset signal). We do this during reprogramming
            // to avoid weird races that make our brains hurt.
            gpio.set_reset(port, 0, pin_mask).unwrap();
        }

        // Reprogramming will continue until morale improves.
        loop {
            let prog = spi.device(ICE40_SPI_DEVICE);
            match reprogram_fpga(&prog, &gpio, &ICE40_CONFIG) {
                Ok(()) => {
                    // yay
                    break;
                }
                Err(_) => {
                    // Try and put state back to something reasonable.
                    // We don't know if we're still locked, so ignore the complaint
                    // if we're not.
                    let _ = prog.release();
                    // We're gonna try again.
                }
            }
        }

        if let Some((port, pin_mask)) = GLOBAL_RESET {
            // Deassert design reset signal. We set the pin, as it's
            // active low.
            gpio.set_reset(port, pin_mask, 0).unwrap();
        }
    }

    // FPGA should now be programmed with the right bitstream.
    loop {
        // TODO this is where, like, sequencer stuff goes
        hl::sleep_for(10);
    }
}

fn reprogram_fpga(
    spi: &spi_api::SpiDevice,
    gpio: &gpio_api::Gpio,
    config: &ice40::Config,
) -> Result<(), ice40::Ice40Error> {
    ice40::begin_bitstream_load(&spi, &gpio, &config)?;

    // We've got the bitstream in Flash, so we can technically just send it in
    // one transaction, but we'll want chunking later -- so let's make sure
    // chunking works.
    let mut bitstream = COMPRESSED_BITSTREAM;
    let mut decompressor = gnarle::Decompressor::default();
    let mut chunk = [0; 256];
    while !bitstream.is_empty() || !decompressor.is_idle() {
        let out =
            gnarle::decompress(&mut decompressor, &mut bitstream, &mut chunk);
        ice40::continue_bitstream_load(&spi, out)?;
    }

    ice40::finish_bitstream_load(&spi, &gpio, &config)
}

static COMPRESSED_BITSTREAM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/fpga.bin.rle"));

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimletlet-2")] {
        const ICE40_SPI_DEVICE: u8 = 0;

        const ICE40_CONFIG: ice40::Config = ice40::Config {
            creset_port: gpio_api::Port::B,
            creset_pin_mask: 1 << 10,
            cdone_port: gpio_api::Port::E,
            cdone_pin_mask: 1 << 15,
        };

        const GLOBAL_RESET: Option<(gpio_api::Port, u16)> = None;

        const FPGA_HACK_PINS: Option<&[(gpio_api::Port, u16, bool)]> = None;

        // On Gimletlet we bring the extra GPIOs out to the uncommitted GPIO
        // headers.
        const ENABLES_PORT: gpio_api::Port = gpio_api::Port::E;
        const ENABLE_V1P2_MASK: u16 = 1 << 2; // J17 pin 2
        const ENABLE_V3P3_MASK: u16 = 1 << 3; // J17 pin 3

        const PGS_PORT: gpio_api::Port = gpio_api::Port::B;
        const PG_V1P2_MASK: u16 = 1 << 14; // J16 pin 2
        const PG_V3P3_MASK: u16 = 1 << 15; // J16 pin 3
        // Gimletlet has no actual regulators onboard, so we pull down to
        // simulate "power not good" until the person hacking on the board
        // installs a jumper or whatever.
        const PGS_PULL: gpio_api::Pull = gpio_api::Pull::Down;
    } else if #[cfg(target_board = "gimlet-1")] {
        const ICE40_SPI_DEVICE: u8 = 1;

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

        // gimlet-1 needs to have a pin flipped to mux the iCE40 SPI flash out
        // of circuit to be able to program the FPGA, because we accidentally
        // share a CS net between Flash and the iCE40.
        //
        // (port, mask, high_flag)
        const FPGA_HACK_PINS: Option<&[(gpio_api::Port, u16, bool)]> = Some(&[
            // SEQ_TO_SEQ_MUX_SEL, pulled high, we drive it low
            (gpio_api::Port::I, 1 << 8, false),
        ]);

        const ENABLES_PORT: gpio_api::Port = gpio_api::Port::A;
        const ENABLE_V1P2_MASK: u16 = 1 << 15;
        const ENABLE_V3P3_MASK: u16 = 1 << 4;

        const PGS_PORT: gpio_api::Port = gpio_api::Port::C;
        const PG_V1P2_MASK: u16 = 1 << 7;
        const PG_V3P3_MASK: u16 = 1 << 6;
        // Gimlet provides external pullups.
        const PGS_PULL: gpio_api::Pull = gpio_api::Pull::None;
    } else if #[cfg(feature = "standalone")] {
        // This is all nonsense to get xtask check to work.

        const ICE40_SPI_DEVICE: u8 = 1;

        const ICE40_CONFIG: ice40::Config = ice40::Config {
            creset_port: gpio_api::Port::D,
            creset_pin_mask: 1 << 5,
            cdone_port: gpio_api::Port::B,
            cdone_pin_mask: 1 << 4,
        };

        const GLOBAL_RESET: Option<(gpio_api::Port, u16)> = Some((
            gpio_api::Port::A,
            1 << 6,
        ));

        const FPGA_HACK_PINS: Option<&[(gpio_api::Port, u16, bool)]> = None;

        const ENABLES_PORT: gpio_api::Port = gpio_api::Port::A;
        const ENABLE_V1P2_MASK: u16 = 1 << 15;
        const ENABLE_V3P3_MASK: u16 = 1 << 4;

        const PGS_PORT: gpio_api::Port = gpio_api::Port::C;
        const PG_V1P2_MASK: u16 = 1 << 7;
        const PG_V3P3_MASK: u16 = 1 << 6;
        const PGS_PULL: gpio_api::Pull = gpio_api::Pull::None;
    } else {
        compiler_error!("unsupported target board");
    }
}
