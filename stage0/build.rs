// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = build_util::out_dir();
    let mut const_file = File::create(out.join("consts.rs")).unwrap();

    let image_id: u64 = build_util::env_var("HUBRIS_IMAGE_ID")?.parse()?;

    writeln!(const_file, "// See build.rs for details")?;

    writeln!(const_file, "#[used]")?;
    writeln!(const_file, "#[no_mangle]")?;
    writeln!(const_file, "#[link_section = \".hubris_id\"]")?;
    writeln!(
        const_file,
        "pub static HUBRIS_IMAGE_ID: u64 = {};",
        image_id
    )?;

    #[cfg(feature = "dice-mfg")]
    gen_memory_range(&out)?;

    Ok(())
}

#[cfg(feature = "dice-mfg")]
fn gen_memory_range(
    out_dir: &std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde::Deserialize;

    #[derive(Deserialize, Debug)]
    struct DiceMfgRegion {
        pub address: u32,
        pub size: u32,
    }

    let toml = build_util::env_var("HUBRIS_DICE_MFG")?;
    let region: DiceMfgRegion = toml::from_str(&toml)?;
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
