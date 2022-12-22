// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

fn main() -> Result<()> {
    let cfg = build_util::task_maybe_config::<Config>()?.unwrap_or_default();

    let allowed_callers = build_util::task_ids()
        .remap_allowed_caller_names_to_ids(&cfg.allowed_callers)?;

    idol::server::build_restricted_server_support(
        "../../idl/jefe.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
        &allowed_callers,
    )
    .unwrap();

    build_util::expose_m_profile();

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("jefe_config.rs");
    let mut out =
        std::fs::File::create(dest_path).context("creating jefe_config.rs")?;

    let task = "hubris_num_tasks::Task";
    {
        let count = cfg.on_state_change.len();

        writeln!(
            out,
            "pub(crate) const MAILING_LIST: [({task}, u32); {count}] = [",
        )?;
        for (name, rec) in cfg.on_state_change {
            writeln!(out, "    ({task}::{name}, 1 << {}),", rec.bit_number)?;
        }
        writeln!(out, "];")?;
    }

    {
        let count = cfg.tasks_to_hold.len();
        writeln!(out, "pub(crate) const HELD_TASKS: [{task}; {count}] = [",)?;
        for name in cfg.tasks_to_hold {
            writeln!(out, "    {task}::{name},")?;
        }
        writeln!(out, "];")?;
    }

    Ok(())
}

/// Jefe task-level configuration.
#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    /// Task requests to be notified on state change, as a map from task name to
    /// `StateChange` record.
    #[serde(default)]
    on_state_change: BTreeMap<String, StateChange>,
    /// Map of operation names to tasks allowed to call them.
    #[serde(default)]
    allowed_callers: BTreeMap<String, Vec<String>>,
    /// Set of names of tasks that should _not_ be automagically restarted on
    /// failure, unless overridden at runtime through Humility.
    #[serde(default)]
    tasks_to_hold: BTreeSet<String>,
}

/// Description of something a task wants done on state change.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct StateChange {
    /// Number of notification bit to signal (_not_ mask).
    bit_number: u8,
}
