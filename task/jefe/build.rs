// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_m_profile();

    idol::server::build_server_support(
        "../../idl/jefe.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let config = build_util::task_config_or_default::<JefeConfig>()?;

    generate_task_table(&config)?;
    
    Ok(())
}

#[derive(Deserialize, Default)]
struct JefeConfig {
    tasks: BTreeMap<String, TaskConfig>,
}

#[derive(Deserialize)]
struct TaskConfig {
    #[serde(default)]
    on_state_change: Option<u32>,
}

fn generate_task_table(
    config: &JefeConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("jefe_task_config.rs");

    let mut out = std::fs::File::create(&dest_path)?;

    // Find tasks that request notification on state change.
    let state_tasks = config.tasks.iter()
        .filter_map(|(name, cfg)| cfg.on_state_change.map(|n| (name, n)));

    writeln!(out, "static NOTIFY_STATE: &[(usize, u32)] = &[")?;
    let task_enum = "hubris_num_tasks::Task";
    for (name, notification) in state_tasks {
        writeln!(out, "    ({}::{} as usize, 0b{:b}),",
            task_enum, name, notification)?;
    }
    writeln!(out, "];")?;

    Ok(())
}
