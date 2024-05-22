// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Result};
use idol::{server::ServerStyle, CounterSettings};

cfg_if::cfg_if! {
    if #[cfg(feature = "dice-seed")] {
        mod config {
            include!("src/config.rs");
        }
        use anyhow::Context;
        use config::DataRegion;
        use std::{fs::File, io::Write};

        const CFG_SRC: &str = "rng-config.rs";
    }
}

#[cfg(feature = "dice-seed")]
fn extern_regions_to_cfg(path: &str) -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join(path);
    let mut out =
        File::create(dest_path).context(format!("creating {}", path))?;

    let data_regions = build_util::task_extern_regions::<DataRegion>()?;
    if data_regions.is_empty() {
        return Err(anyhow!("no data regions found"));
    }

    writeln!(out, "use crate::config::DataRegion;\n\n")?;

    let region = data_regions
        .get("dice_rng")
        .ok_or_else(|| anyhow::anyhow!("dice_certs data region not found"))?;

    Ok(writeln!(
        out,
        r##"pub const DICE_RNG: DataRegion = DataRegion {{
    address: {:#x},
    size: {:#x},
}};"##,
        region.address, region.size
    )?)
}

fn main() -> Result<()> {
    idol::Generator::new()
        .with_counters(CounterSettings::default().with_server_counters(false))
        .build_server_support(
            "../../idl/rng.idol",
            "server_stub.rs",
            ServerStyle::InOrder,
        )
        .map_err(|e| anyhow!(e))?;

    #[cfg(feature = "dice-seed")]
    extern_regions_to_cfg(CFG_SRC)?;

    Ok(())
}
