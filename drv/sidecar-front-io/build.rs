// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_fpga_regmap::fpga_regs;
use sha2::Digest;
use std::{env, fs, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let out_dir = &PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let ecp5_bitstream_name = match env::var("HUBRIS_BOARD")?.as_str() {
        "sidecar-a" => "sidecar_qsfp_x32_controller.bit",
        _ => {
            println!("No FPGA image for target board");
            std::process::exit(1)
        }
    };
    let fpga_bitstream = fs::read(ecp5_bitstream_name)?;
    let compressed_fpga_bitstream = gnarle::compress_to_vec(&fpga_bitstream);

    fs::write(out_dir.join("ecp5.bin.rle"), &compressed_fpga_bitstream)?;

    let out_file = out_dir.join("sidecar_qsfp_x32_controller.rs");
    let mut file = fs::File::create(out_file)?;
    write!(
        &mut file,
        "{}",
        fpga_regs(include_str!("sidecar_qsfp_x32_controller.json"))?
    )?;

    // Calculate a bitstream checksum and add it to the generated Rust file
    let mut hasher = sha2::Sha512::new();
    hasher.update(&compressed_fpga_bitstream);
    let result = hasher.finalize();
    writeln!(
        &mut file,
        "\npub const SIDECAR_IO_BITSTREAM_CHECKSUM: u32 = {:#x};",
        u32::from_le_bytes(result[..4].try_into().unwrap())
    )?;

    // Make sure the app image is rebuilt if the bitstream file for this target
    // changes.
    println!("cargo:rerun-if-changed={}", ecp5_bitstream_name);

    Ok(())
}
