// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_fpga_regmap::fpga_regs;
use std::{fs, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::Generator::default()
        .with_counters(
            cfg!(feature = "counters").then(idol::CounterSettings::default),
        )
        .build_client_stub("../../idl/ignition.idol", "client_stub.rs")?;

    let out_dir = build_util::out_dir();
    let mut reg_map = fs::File::create(out_dir.join("ignition_controller.rs"))?;

    write!(
        &mut reg_map,
        "{}",
        fpga_regs(&std::fs::read_to_string("ignition_controller.json")?)?,
    )?;

    Ok(())
}
