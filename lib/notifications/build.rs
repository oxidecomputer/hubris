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
    for (i, n) in full_task_config.notifications.iter().enumerate() {
        let n = n.to_uppercase().replace("-", "_");
        writeln!(&mut out, "pub const {n}_BIT: u8 = {i};")?;
        writeln!(&mut out, "pub const {n}_MASK: u32 = 1 << {n}_BIT;")?;
    }

    if full_task_config.notifications.len() >= 32 {
        bail!(
            "Too many notifications; \
             overlapping with `INTERNAL_TIMER_NOTIFICATION`"
        );
    }

    for task in build_util::env_var("HUBRIS_TASKS")
        .expect("missing HUBRIS_TASKS")
        .split(",")
    {
        writeln!(&mut out, "pub mod {task} {{")?;
        let full_task_config = build_util::other_task_full_config_toml(task)?;
        for (i, n) in full_task_config.notifications.iter().enumerate() {
            let n = n.to_uppercase().replace("-", "_");
            writeln!(&mut out, "    pub const {n}_BIT: u8 = {i};")?;
            writeln!(&mut out, "    pub const {n}_MASK: u32 = 1 << {n}_BIT;")?;
        }
        writeln!(&mut out, "}}")?;
    }

    if full_task_config.notifications.len() >= 32 {
        bail!(
            "Too many notifications; \
             overlapping with `INTERNAL_TIMER_NOTIFICATION`"
        );
    }

    Ok(())
}
