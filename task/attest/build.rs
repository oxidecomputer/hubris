// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use idol::{server::ServerStyle, CounterSettings};
use serde::Deserialize;
use std::{fs::File, io::Write};

mod config {
    include!("src/config.rs");
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskConfig {
    permit_log_reset: Vec<String>,
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
    let mut out = File::create(dest_path)
        .with_context(|| format!("creating {}", CFG_SRC))?;

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

    // Get list of those tasks allowed to reset the attestation log
    if let Ok(task_config) = build_util::task_config::<TaskConfig>() {
        writeln!(
            out,
            "pub const PERMIT_LOG_RESET: [u16; {}] = [",
            task_config.permit_log_reset.len()
        )?;
        let tasks = build_util::task_ids();
        for task_name in task_config.permit_log_reset {
            let id = tasks.get(task_name.as_str()).with_context(|| {
                format!("attest: allow_reset_task '{task_name}' is not present")
            })?;
            writeln!(out, "{id}, // Allow {task_name}")?;
        }
        writeln!(out, "];")?;
    } else {
        writeln!(out, "pub const PERMIT_LOG_RESET: [u16; 0] = [];")?;
    }

    Ok(())
}
