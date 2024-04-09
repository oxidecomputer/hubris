// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use std::io::Write;

#[derive(serde::Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
struct DumpRegion {
    pub address: u32,
    pub size: u32,
}

fn main() -> anyhow::Result<()> {
    build_util::expose_target_board();

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("dumptruck_generated.rs");
    let mut out = std::fs::File::create(&dest_path)
        .with_context(|| format!("failed to create {}", dest_path.display()))?;
    output_dump_areas(&mut out).context("failed to generate dump areas")?;

    Ok(())
}

/// Output our dump areas, making the assumption that any extern regions in use
/// by this task are in fact alternate RAMs for dump areas.  Because this assumption
/// is a tad aggressive, we also make sure that those extern regions match
/// exactly what the dump agent itself is using.
///
fn output_dump_areas(out: &mut impl Write) -> Result<()> {
    let task = build_util::task_name();
    let dump_regions = build_util::task_extern_regions::<DumpRegion>()?;

    if dump_regions.is_empty() {
        anyhow::bail!(
            "{task} is configured for dumping, but no dump regions have been \
            specified via extern_regions"
        )
    }

    let me = build_util::task_full_config_toml()?;
    let dump_agent = build_util::other_task_full_config_toml("dump_agent")
        .with_context(|| {
            format!(
            "{task} is configured for task dumping, but can't find dump_agent",
        )
        })?;

    if me.extern_regions != dump_agent.extern_regions {
        anyhow::bail!(
            "{task} is configured for task dumping, but extern regions for \
            {task} ({:?}) do not match extern regions for dump agent ({:?})",
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
