// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use path_slash::PathBufExt;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let toml = Config::from_file(&cfg)?;

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
                .arg(out.join("final.ihex"));

            let mut reset = Command::new("pyocd");
            reset.arg("reset").arg("-t").arg("lpc55s69");

            if verbose {
                flash.arg("-v");
                reset.arg("-v");
            }

            if let Ok(id) = env::var("PYOCD_PROBE_ID") {
                flash.arg("-u");
                flash.arg(&id);
                reset.arg("-u");
                reset.arg(&id);
            }

            (flash, Some(reset))
        }
        "gemini-bu-rot-1" | "gimlet-rot-1" => {
            let mut flash = Command::new("pyocd");
            flash
                .arg("flash")
                .arg("-t")
                .arg("lpc55s28")
                .arg("--format")
                .arg("hex")
                .arg(out.join("final.ihex"));

            let mut reset = Command::new("pyocd");
            reset.arg("reset").arg("-t").arg("lpc55s28");

            if verbose {
                flash.arg("-v");
                reset.arg("-v");
            }

            if let Ok(id) = env::var("PYOCD_PROBE_ID") {
                flash.arg("-u");
                flash.arg(&id);
                reset.arg("-u");
                reset.arg(&id);
            }

            (flash, Some(reset))
        }
        "stm32f3-discovery" | "stm32f4-discovery" | "nucleo-h743zi2"
        | "nucleo-h753zi" | "stm32h7b3i-dk" | "gemini-bu-1" | "gimletlet-1"
        | "gimletlet-2" | "gimlet-1" | "psc-1" | "sidecar-1" | "stm32g031"
        | "stm32g070" | "stm32g0b1" => {
            let cfg = if toml.board == "stm32f3-discovery" {
                "./app/demo-stm32f4-discovery/openocd-f3.cfg"
            } else if toml.board == "stm32f4-discovery" {
                "./app/demo-stm32f4-discovery/openocd.cfg"
            } else if toml.board == "stm32g031" {
                "./app/demo-stm32g0-nucleo/openocd.cfg"
            } else if toml.board == "stm32g070" {
                "./app/demo-stm32g0-nucleo/openocd.cfg"
            } else if toml.board == "stm32g0b1" {
                "./app/demo-stm32g0-nucleo/openocd.cfg"
            } else if toml.board == "gemini-bu-1" {
                "./app/gemini-bu/openocd.cfg"
            } else if toml.board == "gimletlet-2" {
                "./app/gimletlet/openocd.cfg"
            } else {
                "./app/demo-stm32h7-nucleo/openocd.cfg"
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
                    out.join("final.srec").to_slash().unwrap()
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
