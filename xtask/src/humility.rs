use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(cfg: &Path, options: &Vec<String>) -> anyhow::Result<()> {
    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut archive = PathBuf::from("target");
    archive.push(&toml.name);
    archive.push("dist");
    archive.push(format!("build-{}.zip", &toml.name));

    let mut humility = Command::new("humility");
    humility.arg("-a").arg(archive);

    for opt in options {
        humility.arg(opt);
    }

    let status = humility
        .status()
        .with_context(|| format!("failed to run humility ({:?})", humility))?;

    if !status.success() {
        anyhow::bail!("humility failed");
    }

    Ok(())
}
