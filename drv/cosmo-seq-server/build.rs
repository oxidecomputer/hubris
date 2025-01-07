// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{fs, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("cosmo_fpga.rs");
    let mut file = fs::File::create(out_file)?;

    // Check that a valid bitstream is available for this board.
    let board = build_util::env_var("HUBRIS_BOARD")?;
    if board != "cosmo-a" {
        panic!("unknown target board");
    }

    // Pull the bitstream checksum from an environment variable
    // (injected by `xtask` itself as part of auxiliary flash packing)
    let checksum =
        build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM_ICE4").unwrap();
    writeln!(
        &mut file,
        "\npub const FRONT_FPGA_BITSTREAM_CHECKSUM: [u8; 32] = {};",
        checksum,
    )?;

    idol::Generator::new().build_server_support(
        "../../idl/cpu-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}
