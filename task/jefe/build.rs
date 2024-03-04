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

    idol::Generator::new()
        .build_restricted_server_support(
            "../../idl/jefe.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
            &allowed_callers,
        )
        .unwrap();

    build_util::expose_target_board();
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
                rec.to_ascii_uppercase().replace('-', "_"),
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
struct DumpRegion {
    pub address: u32,
    pub size: u32,
}

///
/// Output our dump areas, making the assumption that any extern regions in use
/// by Jefe are in fact alternate RAMs for dump areas.  Because this assumption
/// is a tad aggressive, we also make sure that those extern regions match
/// exactly what the dump agent itself is using.
///
#[cfg(feature = "dump")]
fn output_dump_areas(out: &mut std::fs::File) -> Result<()> {
    let dump_regions = build_util::task_extern_regions::<DumpRegion>()?;

    if dump_regions.is_empty() {
        anyhow::bail!(
            "jefe is configured for dumping, but no dump regions have been \
            specified via extern_regions"
        )
    }

    let me = build_util::task_full_config_toml()?;
    let dump_agent = build_util::other_task_full_config_toml("dump_agent")
        .context(
            "jefe is configured for task dumping, but can't find dump_agent",
        )?;

    if me.extern_regions != dump_agent.extern_regions {
        anyhow::bail!(
            "jefe is configured for task dumping, but extern regions for \
             jefe ({:?}) do not match extern regions for dump agent ({:?})",
            me.extern_regions,
            dump_agent.extern_regions
        );
    }

    write!(
        out,
        "pub(crate) const DUMP_AREAS: [humpty::DumpAreaRegion; {}] = [",
        dump_regions.len(),
    )?;

    let mut min = dump_regions[0].address;
    let mut max = dump_regions[0].address + dump_regions[0].size;

    for (name, region) in &dump_regions {
        let address = region.address;
        let length = region.size;

        //
        // Determine our minimium and maximum dump area addresses so we
        // can generate constants to short-circuit the determination of
        // a given address being outside of any dump area.
        //
        min = std::cmp::min(address, min);
        max = std::cmp::max(address + length, max);

        writeln!(
            out,
            r##"
    // {name} dump area
    humpty::DumpAreaRegion {{
        address: {address:#x},
        length: {length:#x},
    }},"##
        )?;
    }

    writeln!(out, "];")?;

    writeln!(
        out,
        r##"
pub(crate) const DUMP_ADDRESS_MIN: u32 = {min:#x};
pub(crate) const DUMP_ADDRESS_MAX: u32 = {max:#x};"##
    )?;

    Ok(())
}
