// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut archive = PathBuf::from("target");
    archive.push(&toml.name);
    archive.push("dist");
    archive.push(format!("build-{}.zip", &toml.name));

    let mut humility = Command::new("humility");
    humility.arg("-a").arg(archive);

    if verbose {
        humility.arg("-v");
    }

    humility.arg("test");

    let status = humility
        .status()
        .with_context(|| format!("failed to run humility ({:?})", humility))?;

    if !status.success() {
        anyhow::bail!("test failed");
    }

    Ok(())
}
