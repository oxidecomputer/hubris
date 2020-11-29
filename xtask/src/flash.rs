use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    println!("{:?}", toml);

    let mut out = PathBuf::from("target");
    out.push(toml.name);
    out.push("dist");

    let (mut flash, mut reset) = match toml.board.as_str() {
        "lpcxpresso55s69" => {
            let mut flash = Command::new("pyocd");
            flash
                .arg("flash")
                .arg("-t")
                .arg("lpc55s69")
                .arg("--format")
                .arg("hex")
                .arg(out.join("combined.ihex"));

            if verbose {
                flash.arg("-v");
            }

            let mut reset = Command::new("pyocd");
            reset.arg("reset").arg("-t").arg("lpc55s69");

            (flash, reset)
        }
        _ => {
            anyhow::bail!("unrecognized board {}", toml.board);
        }
    };

    let status = flash
        .status()
        .with_context(|| format!("failed to flash ({:?})", flash))?;

    if !status.success() {
        anyhow::bail!("flash command ({:?}) failed; see output", flash);
    }

    let status = reset
        .status()
        .with_context(|| format!("failed to reset ({:?})", reset))?;

    if !status.success() {
        anyhow::bail!("reset command ({:?}) failed; see output", reset);
    }

    Ok(())
}
