// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Config;

pub fn run(cfg: &Path, gdb_cfg: &Path) -> anyhow::Result<()> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut out = PathBuf::from("target");
    out.push(toml.name);
    out.push("dist");

    let gdb_path = out.join("script.gdb");
    let combined_path = out.join("final.elf");

    let mut cmd = None;

    const GDB_NAMES: [&str; 2] = ["arm-none-eabi-gdb", "gdb-multiarch"];
    for candidate in &GDB_NAMES {
        if Command::new(candidate).arg("--version").status().is_ok() {
            cmd = Some(Command::new(candidate));
            break;
        }
    }

    let mut cmd =
        cmd.ok_or(anyhow::anyhow!("GDB not found.  Tried: {:?}", GDB_NAMES))?;

    cmd.arg("-q")
        .arg("-x")
        .arg(gdb_path)
        .arg("-x")
        .arg(&gdb_cfg)
        .arg(combined_path);

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("command failed, see output for details");
    }

    Ok(())
}
