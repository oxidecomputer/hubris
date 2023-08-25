// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::fs::File;
use std::io::Write;

#[derive(Deserialize, Debug)]
struct Region {
    pub name: String,
    pub address: u32,
    pub size: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let outputs = build_util::env_var("HUBRIS_FLASH_OUTPUTS")?;
    let outputs: Vec<Region> = ron::de::from_str(&outputs)?;

    let out_dir = build_util::out_dir();
    let mut cfg = File::create(out_dir.join("config.rs")).unwrap();

    writeln!(cfg, "use core::ops::Range;\n")?;
    for region in outputs {
        writeln!(
            cfg,
            "#[allow(dead_code)]\npub const FLASH_{}: Range<u32> = {:#x}..{:#x};",
            region.name.replace("-", "_").to_uppercase(),
            region.address,
            region.address + region.size
        )?;
    }

    Ok(())
}
