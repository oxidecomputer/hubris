// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{fs, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("spartan7_fpga.rs");
    let mut file = fs::File::create(out_file)?;

    // Check that a valid bitstream is available for this board.
    let board = build_util::target_board().expect("could not get target board");
    match board.as_str() {
        "grapefruit" | "cosmo-a" | "cosmo-b" => (),
        _ => panic!("unknown target board '{board}'"),
    }

    // Pull the bitstream checksum from an environment variable
    // (injected by `xtask` itself as part of auxiliary flash packing)
    let checksum =
        build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM_SPA7").unwrap();
    writeln!(
        &mut file,
        "\npub const SPARTAN7_FPGA_BITSTREAM_CHECKSUM: [u8; 32] = {checksum};",
    )?;

    idol::Generator::new().build_server_support(
        "../../idl/spartan7-loader.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}
