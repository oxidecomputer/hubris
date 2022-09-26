// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let global_config = build_util::config::<GlobalConfig>()?;
    generate_auxflash_config(&global_config.auxflash)?;

    idol::client::build_client_stub(
        "../../idl/auxflash.idol",
        "client_stub.rs",
    )?;
    Ok(())
}

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct GlobalConfig {
    auxflash: AuxFlashConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct AuxFlashConfig {
    memory_size: u32,
    slot_count: u32,
}

fn generate_auxflash_config(
    config: &AuxFlashConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("auxflash_config.rs");

    let mut out = std::fs::File::create(&dest_path)?;

    writeln!(out, "pub const MEMORY_SIZE: u32 = {};", config.memory_size)?;
    writeln!(out, "pub const SLOT_COUNT: u32 = {};", config.slot_count)?;

    Ok(())
}
