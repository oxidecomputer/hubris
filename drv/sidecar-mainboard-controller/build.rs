// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_fpga_regmap::fpga_regs;
use std::{env, fs, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    if env::var("HUBRIS_BOARD")? != "sidecar-a" {
        panic!("unknown target board");
    }

    let out_dir = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let out_file = out_dir.join("sidecar_mainboard_controller.rs");
    let mut file = fs::File::create(out_file)?;
    write!(
        &mut file,
        "{}",
        fpga_regs(include_str!("sidecar_mainboard_controller.json"))?,
    )?;

    // Pull the bitstream checksum from an environment variable
    // (injected by `xtask` itself as part of auxiliary flash packing)
    let checksum = env!("HUBRIS_AUXFLASH_CHECKSUM_FPGA");
    println!("cargo:rerun-if-env-changed=HUBRIS_AUXFLASH_CHECKSUM_FPGA");
    writeln!(
        &mut file,
        "\npub const SIDECAR_MAINBOARD_BITSTREAM_CHECKSUM: [u8; 32] = {};",
        checksum,
    )?;
    Ok(())
}
