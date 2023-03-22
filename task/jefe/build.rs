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
    build_util::build_notifications()?;

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
            writeln!(
                out,
                "    ({task}::{name}, crate::notifications::{name}::{}_MASK),",
                rec.to_ascii_uppercase().replace("-", "_"),
            )?;
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

    #[cfg(feature = "dump")]
    output_dump_areas(&mut out)?;
    Ok(())
}

/// Jefe task-level configuration.
#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    /// Task requests to be notified on state change, as a map from task name to
    /// notification name (in the target task)
    #[serde(default)]
    on_state_change: BTreeMap<String, String>,
    /// Map of operation names to tasks allowed to call them.
    #[serde(default)]
    allowed_callers: BTreeMap<String, Vec<String>>,
    /// Set of names of tasks that should _not_ be automagically restarted on
    /// failure, unless overridden at runtime through Humility.
    #[serde(default)]
    tasks_to_hold: BTreeSet<String>,
}

#[cfg(feature = "dump")]
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
struct Peripheral {
    pub address: u32,
    pub size: u32,
}

///
/// Output our dump areas, making the assumption that any peripherals in use
/// by Jefe are in fact alternate RAMs for dump areas.  Because this assumption
/// is a tad aggressive, we also make sure that the peripheral areas match
/// exactly what the dump agent itself is using.
///
#[cfg(feature = "dump")]
fn output_dump_areas(out: &mut std::fs::File) -> Result<()> {
    let peripherals = build_util::task_peripherals::<Peripheral>()?;

    let me = build_util::task_full_config_toml()?;
    let dump_agent = build_util::other_task_full_config_toml("dump_agent")
        .context(
            "jefe is configured for task dumping, but can't find dump_agent",
        )?;

    if me.uses != dump_agent.uses {
        anyhow::bail!(
            "jefe is configured for task dumping, but peripherals used by \
             jefe ({:?}) do not match peripherals used by dump agent ({:?})",
            me.uses,
            dump_agent.uses
        );
    }

    write!(
        out,
        "pub(crate) const DUMP_AREAS: [humpty::DumpArea; {}] = [",
        peripherals.len(),
    )?;

    for (name, peripheral) in &peripherals {
        let address = peripheral.address;
        let length = peripheral.size;

        writeln!(
            out,
            r##"
    // {name} dump area
    humpty::DumpArea {{
        address: {address:#x},
        length: {length:#x},
        contents: humpty::DumpContents::Available,
    }},"##
        )?;
    }

    writeln!(out, "];")?;

    Ok(())
}
