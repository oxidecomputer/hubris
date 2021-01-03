use path_slash::PathBufExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut out = PathBuf::from("target");
    out.push(toml.name);
    out.push("dist");

    let (mut flash, reset) = match toml.board.as_str() {
        "lpcxpresso55s69" => {
            let mut flash = Command::new("pyocd");
            flash
                .arg("flash")
                .arg("-t")
                .arg("lpc55s69")
                .arg("--format")
                .arg("hex")
                .arg(out.join("combined.ihex"));

            let mut reset = Command::new("pyocd");
            reset.arg("reset").arg("-t").arg("lpc55s69");

            if verbose {
                flash.arg("-v");
                reset.arg("-v");
            }

            (flash, Some(reset))
        }
        "stm32f4-discovery" | "nucleo-h743zi2" | "stm32h7b3i-dk"
        | "gemini-bu-1" => {
            let cfg = if toml.board == "stm32f4-discovery" {
                "./demo/openocd.cfg"
            } else if toml.board == "gemini-bu-1" {
                "./gemini-bu/openocd.cfg"
            } else {
                "./demo-stm32h7/openocd.cfg"
            };

            let mut flash = Command::new("openocd");

            // Note that OpenOCD only deals with slash paths, not native paths
            // (that is, its file arguments are forward-slashed delimited even
            // when/where the path separator is a back-slash) -- so we be sure
            // to always give it a slash path for the `program` argument.
            flash
                .arg("-f")
                .arg(cfg)
                .arg("-c")
                .arg(format!(
                    "program {} verify reset",
                    out.join("combined.srec").to_slash().unwrap()
                ))
                .arg("-c")
                .arg("exit");

            (flash, None)
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

    if let Some(mut reset) = reset {
        let status = reset
            .status()
            .with_context(|| format!("failed to reset ({:?})", reset))?;

        if !status.success() {
            anyhow::bail!("reset command ({:?}) failed; see output", reset);
        }
    }

    Ok(())
}
