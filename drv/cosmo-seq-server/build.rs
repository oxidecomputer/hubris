// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{fs, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    build_util::build_notifications()?;
    build_stm32xx_sys::build_gpio_irq_pins()?;

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("cosmo_fpga.rs");
    let mut file = fs::File::create(out_file)?;

    // Check that a valid bitstream is available for this board.
    let board = build_util::env_var("HUBRIS_BOARD")?;
    if board != "cosmo-a" {
        panic!("unknown target board");
    }

    // Pull the bitstream checksums from environment variables
    // (injected by `xtask` itself as part of auxiliary flash packing)
    let ice40_checksum =
        build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM_ICE4").unwrap();
    writeln!(
        &mut file,
        "\npub const FRONT_FPGA_BITSTREAM_CHECKSUM: [u8; 32] = {ice40_checksum};",
    )?;
    let spartan7_checksum =
        build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM_SPA7").unwrap();
    writeln!(
        &mut file,
        "\npub const SPARTAN7_FPGA_BITSTREAM_CHECKSUM: [u8; 32] = {spartan7_checksum};",
    )?;

    idol::Generator::new().build_server_support(
        "../../idl/cpu-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let out_file = out_dir.join("fmc_sequencer.rs");
    let mut file = std::fs::File::create(out_file)?;
    for periph in ["sequencer", "info"] {
        write!(
            &mut file,
            "{}",
            build_fpga_regmap::fpga_peripheral(
                periph,
                "drv_spartan7_loader_api::Spartan7Token"
            )?
        )?;
    }

    let disposition = build_i2c::Disposition::Devices;
    if let Err(e) = build_i2c::codegen(disposition) {
        println!("cargo::error=I2C code generation failed: {e}",);
        std::process::exit(1);
    }

    Ok(())
}
