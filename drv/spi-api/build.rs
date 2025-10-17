// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Result};
use build_spi::SpiGlobalConfig;
use std::fs::File;
use std::io::Write;

fn main() -> Result<()> {
    idol::Generator::new()
        .with_op_enum_derives(std::iter::once("counters::Count"))?
        .build_client_stub("../../idl/spi.idol", "client_stub.rs")
        .map_err(|e| anyhow!(e))?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("spi_devices.rs");
    let mut file = File::create(dest_path)?;

    let global_config = build_util::config::<SpiGlobalConfig>()?;

    writeln!(&mut file, "pub mod devices {{")?;
    for (periph, p) in global_config.spi {
        writeln!(&mut file, "    // {periph} ({} devices)", p.devices.len())?;
        for (i, name) in p.devices.keys().enumerate() {
            let name = name.to_uppercase();
            writeln!(&mut file, "    pub const {name}: u8 = {i};")?;
        }
    }
    writeln!(&mut file, "}}")?;

    Ok(())
}
