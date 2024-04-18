// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use build_fpga_regmap::fpga_regs;
use std::{fs, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();

    let board = build_util::env_var("HUBRIS_BOARD")?;
    if board != "sidecar-b"
        && board != "sidecar-c"
        && board != "sidecar-d"
        && board != "medusa-a"
    {
        panic!("unknown target board");
    }
    idol::Generator::new()
        .with_counters(idol::CounterSettings::default())
        .build_client_stub("../../idl/front-io.idol", "client_stub.rs")?;

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("sidecar_qsfp_x32_controller_regs.rs");
    let mut file = fs::File::create(out_file)?;
    write!(
        &mut file,
        "{}",
        fpga_regs(include_str!("sidecar_qsfp_x32_controller_regs.json"))?
    )?;

    // Pull the bitstream checksum from an environment variable
    // (injected by `xtask` itself as part of auxiliary flash packing)
    let checksum =
        build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM_QSFP").unwrap();
    writeln!(
        &mut file,
        "\npub const SIDECAR_IO_BITSTREAM_CHECKSUM: [u8; 32] = {};",
        checksum,
    )?;

    Ok(())
}
