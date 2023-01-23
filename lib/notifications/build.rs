// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Result};
use std::io::Write;

fn main() -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("notifications_config.rs");
    let mut out = std::fs::File::create(dest_path)?;

    let full_task_config = build_util::task_full_config_toml()?;

    if full_task_config.notifications.len() >= 32 {
        bail!(
            "Too many notifications; \
             overlapping with `INTERNAL_TIMER_NOTIFICATION`"
        );
    }
    if full_task_config.name == "task-jefe"
        && full_task_config.notifications.get(0).cloned()
            != Some("fault".to_string())
    {
        bail!("`jefe` must have \"fault\" as its first notification");
    }

    write_task_notifications(&mut out, &full_task_config.notifications)?;

    for task in build_util::env_var("HUBRIS_TASKS")
        .expect("missing HUBRIS_TASKS")
        .split(",")
    {
        let full_task_config = build_util::other_task_full_config_toml(task)?;
        writeln!(&mut out, "pub mod {task} {{")?;
        write_task_notifications(&mut out, &full_task_config.notifications)?;
        writeln!(&mut out, "}}")?;
    }

    Ok(())
}

fn write_task_notifications<W: Write>(out: &mut W, t: &[String]) -> Result<()> {
    for (i, n) in t.iter().enumerate() {
        let n = n.to_uppercase().replace('-', "_");
        writeln!(out, "pub const {n}_BIT: u8 = {i};")?;
        writeln!(out, "pub const {n}_MASK: u32 = 1 << {n}_BIT;")?;
    }
    Ok(())
}
