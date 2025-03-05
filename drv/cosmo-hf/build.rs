// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{io::Write, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    build_util::build_notifications()?;

    idol::Generator::new().build_server_support(
        "../../idl/hf.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("fmc_periph.rs");
    let mut file = std::fs::File::create(out_file)?;

    // Pick out the right JSON files for our FPGA image
    let board = build_util::target_board().expect("could not get target board");
    let (f, short) = match board.as_str() {
        "grapefruit" => ("grapefruit", "gfruit"),
        "cosmo-a" => ("cosmo-seq", "cosmo_seq"),
        _ => panic!("unknown board '{board}'"),
    };
    let folder = format!("../spartan7-loader/{f}");
    let base_path = Path::new(&folder);

    let node = base_path.join("spi_nor_reg_map.json");
    let top = base_path.join(format!("{short}_top_map.json"));
    let token = "drv_spartan7_loader_api::Spartan7Token";
    write!(
        &mut file,
        "{}",
        build_fpga_regmap::fpga_peripheral(&node, &top, 0x60000000, token)?
    )?;

    let node = base_path.join("info_regs.json");
    write!(
        &mut file,
        "{}",
        build_fpga_regmap::fpga_peripheral(&node, &top, 0x60000000, token)?
    )?;

    Ok(())
}
