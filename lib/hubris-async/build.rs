// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result, bail};
use std::io::Write;

/// Name of the notification the executor uses for its timer.
const TIMER_NOTIFICATION: &str = "timer";

fn main() -> Result<()> {
    println!("cargo::rerun-if-env-changed=HUBRIS_TASK_CONFIG");

    let out_dir = std::env::var("OUT_DIR")?;
    let mut out = std::fs::File::create(
        std::path::Path::new(&out_dir).join("config.rs"),
    )?;

    // Outside of `cargo xtask` (e.g. a bare `cargo check`, or some editor
    // setups) there's no task to check against. Allow the build so tooling
    // keeps working; such a build can't produce a runnable image anyway.
    if std::env::var_os("HUBRIS_TASK_CONFIG").is_none() {
        writeln!(out, "pub(crate) const TIMER_MASK: Option<u32> = None;")?;
        return Ok(());
    }

    let task = build_util::task_full_config_toml()?;
    let name = std::env::var("HUBRIS_TASK_NAME").unwrap_or(task.name.clone());

    if !task.is_async {
        bail!(
            "task `{name}` depends on hubris-async, but doesn't set \
             `async = true` in its app.toml entry. Async tasks are linted \
             against syscalls that interfere with the executor, so they must \
             be declared as such."
        );
    }

    let bit = task.notification_bit(TIMER_NOTIFICATION).with_context(|| {
        format!(
            "async task `{name}` needs a `{TIMER_NOTIFICATION}` notification \
             for the executor's timer"
        )
    })?;
    writeln!(
        out,
        "pub(crate) const TIMER_MASK: Option<u32> = Some(1 << {bit});"
    )?;

    Ok(())
}
