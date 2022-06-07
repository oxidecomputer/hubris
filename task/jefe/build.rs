// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;

fn main() -> Result<()> {
    idol::server::build_server_support(
        "../../idl/jefe.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )
    .unwrap();

    build_util::expose_m_profile();

    let cfg = build_util::task_maybe_config::<Config>()?.unwrap_or_default();
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("jefe_config.rs");
    let mut out =
        std::fs::File::create(&dest_path).context("creating jefe_config.rs")?;

    let count = cfg.on_state_change.len();

    let task = "hubris_num_tasks::Task";
    writeln!(
        out,
        "pub(crate) const MAILING_LIST: [({}, u32); {}] = [",
        task, count
    )?;
    for (name, rec) in cfg.on_state_change {
        writeln!(out, "    ({}::{}, 1 << {})", task, name, rec.bit_number)?;
    }
    writeln!(out, "];")?;

    Ok(())
}

/// Jefe task-level configuration.
#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct Config {
    /// Task requests to be notified on state change, as a map from task name to
    /// `StateChange` record.
    on_state_change: BTreeMap<String, StateChange>,
}

/// Description of something a task wants done on state change.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct StateChange {
    /// Number of notification bit to signal (_not_ mask).
    bit_number: u8,
}
