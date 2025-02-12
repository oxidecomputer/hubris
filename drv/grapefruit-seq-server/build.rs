// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use std::{io::Write, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    idol::Generator::new().build_server_support(
        "../../idl/cpu-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let out_dir = build_util::out_dir();
    let out_file = out_dir.join("fmc_sgpio.rs");
    let mut file = std::fs::File::create(out_file)?;
    let node = Path::new("../spartan7-loader/grapefruit/sgpio_reg_map.json");
    let top = Path::new("../spartan7-loader/grapefruit/gfruit_top_map.json");
    let token = "drv_spartan7_loader_api::Spartan7Token";
    write!(
        &mut file,
        "{}",
        build_fpga_regmap::fpga_peripheral(node, top, 0x60000000, token)?
    )?;

    Ok(())
}
