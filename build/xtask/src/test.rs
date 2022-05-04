// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    let toml = Config::from_file(&cfg)?;

    let mut archive = PathBuf::from("target");
    archive.push(&toml.name);
    archive.push("dist");
    archive.push(format!("build-{}.zip", &toml.name));

    let humility_path = match env::var("HUBRIS_HUMILITY_PATH") {
        Ok(path) => path,
        _ => "humility".to_string(),
    };

    let mut humility = Command::new(humility_path);
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
