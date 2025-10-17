// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    build_util::build_notifications()?;

    let board = build_util::env_var("HUBRIS_BOARD")?;
    if board != "cosmo-a" && board != "cosmo-b" {
        panic!("unknown target board");
    }

    idol::Generator::new().build_server_support(
        "../../idl/ignition-flash.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("fmc_sequencer.rs");
    let mut file = std::fs::File::create(out_file)?;
    write!(
        &mut file,
        "{}",
        build_fpga_regmap::fpga_peripheral(
            "sequencer",
            "drv_spartan7_loader_api::Spartan7Token"
        )?
    )?;

    Ok(())
}
