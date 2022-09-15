// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_fpga_regmap::fpga_regs;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let out_dir = &PathBuf::from(env::var_os("OUT_DIR").unwrap());

    fs::write(
        out_dir.join("sidecar_mainboard_controller.rs"),
        fpga_regs(include_str!("sidecar_mainboard_controller.json"))?,
    )?;

    let ecp5_bitstream_name = match env::var("HUBRIS_BOARD")?.as_str() {
        "gimletlet-2" => "sidecar_mainboard_emulator_ecp5_evn.bit",
        "sidecar-a" => "sidecar_mainboard_controller.bit",
        _ => {
            println!("No FPGA image for target board");
            std::process::exit(1)
        }
    };
    let fpga_bitstream = fs::read(ecp5_bitstream_name)?;
    let compressed_fpga_bitstream = gnarle::compress_to_vec(&fpga_bitstream);

    fs::write(out_dir.join("ecp5.bin.rle"), &compressed_fpga_bitstream)?;

    // Make sure the app image is rebuilt if the bitstream file for this target
    // changes.
    println!("cargo:rerun-if-changed={}", ecp5_bitstream_name);

    Ok(())
}
