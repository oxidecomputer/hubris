// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("auxflash_config.rs");

    let mut out = std::fs::File::create(dest_path)?;

    // Check that the config is reasonable:
    // a. We have at least 6 slots (see RFD 311)
    assert!(config.slot_count >= 6, "auxflash requires at least 6 slots");
    // b. Memory size is evenly divisible by the slot count
    assert_eq!(
        config.memory_size % config.slot_count,
        0,
        "auxflash memory must be evenly divisble by slot count"
    );
    // c. Slot offsets are page-aligned (assuming 64 KiB pages; we can update
    //    this as needed)
    assert_eq!(
        config.memory_size / config.slot_count % (64 << 10),
        0,
        "auxflash slots must be page aligned"
    );

    writeln!(out, "pub const MEMORY_SIZE: u32 = {};", config.memory_size)?;
    writeln!(out, "pub const SLOT_COUNT: u32 = {};", config.slot_count)?;

    Ok(())
}
