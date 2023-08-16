// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = build_util::out_dir();
    gen_image_flash_range(&out)?;

    #[cfg(feature = "dice-mfg")]
    {
        gen_memory_range(&out)?;
    }

    Ok(())
}

#[derive(Deserialize, Debug)]
struct Region {
    pub name: String,
    pub address: u32,
    pub size: u32,
}

#[cfg(feature = "dice-mfg")]
fn gen_memory_range(
    out_dir: &std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let toml = build_util::env_var("HUBRIS_DICE_MFG")?;
    let region: Region = toml::from_str(&toml)?;
    let mut dice_mfg = File::create(out_dir.join("dice-mfg.rs")).unwrap();

    writeln!(
        dice_mfg,
        "use core::ops::Range;\n\n\
             pub const DICE_FLASH: Range<usize> = {:#x}..{:#x};",
        region.address,
        region.address + region.size
    )?;

    Ok(())
}

fn gen_image_flash_range(
    out_dir: &std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let outputs = build_util::env_var("HUBRIS_FLASH_OUTPUTS")?;
    let outputs: Vec<Region> = ron::de::from_str(&outputs)?;

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
