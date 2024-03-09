// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use idol::{server::ServerStyle, CounterSettings};
use std::{fs::File, io::Write};

mod config {
    include!("src/config.rs");
}

use config::DataRegion;

const CFG_SRC: &str = "attest-config.rs";

fn main() -> Result<()> {
    idol::Generator::new()
        .with_counters(CounterSettings::default().with_server_counters(false))
        .build_server_support(
            "../../idl/attest.idol",
            "server_stub.rs",
            ServerStyle::InOrder,
        )
        .unwrap();

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join(CFG_SRC);
    let mut out =
        File::create(dest_path).context(format!("creating {}", CFG_SRC))?;

    let data_regions = build_util::task_extern_regions::<DataRegion>()?;
    if data_regions.is_empty() {
        return Err(anyhow::anyhow!("no data regions found"));
    }

    let region = data_regions
        .get("dice_certs")
        .ok_or_else(|| anyhow::anyhow!("dice_certs data region not found"))?;
    writeln!(
        out,
        r##"use crate::config::DataRegion;
pub const CERT_DATA: DataRegion = DataRegion {{
    address: {:#x},
    size: {:#x},
}};"##,
        region.address, region.size
    )?;

    let region = data_regions
        .get("dice_alias")
        .ok_or_else(|| anyhow::anyhow!("dice_alias data region not found"))?;
    writeln!(
        out,
        r##"
pub const ALIAS_DATA: DataRegion = DataRegion {{
    address: {:#x},
    size: {:#x},
}};"##,
        region.address, region.size
    )?;

    Ok(())
}
