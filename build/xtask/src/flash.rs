// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use path_slash::PathBufExt;
use serde::Serialize;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::Config;

//
// We allow for enough information to be put in the archive for the image to
// be flashed based only on the archive (e.g., by Humility).  Because flashing
// is itself a bit of a mess (requiring different programs for different
// targets), this is a bit gritty (e.g., any required external configuration
// files must themselves put in the archive).  If these structures need to
// change, be sure to make corresponding changes to Humility.
//
#[derive(Debug, Serialize)]
pub enum FlashProgram {
    PyOcd(Vec<FlashArgument>),
    OpenOcd(FlashProgramConfig),
}

//
// Enum describing flash programs configuration (e.g., "openocd.cfg" for
// OpenOCD), either as a path in the file system or with the entire contents.
//
#[derive(Debug, Serialize)]
pub enum FlashProgramConfig {
    Path(Vec<String>),
    Payload(String),
}

//
// An enum describing a single command-line argument to the flash program.
//
#[derive(Debug, Serialize)]
pub enum FlashArgument {
    // A direct string
    Direct(String),

    // The filesystem path of the binary flash payload itself
    Payload,

    // A single argument consisting of a prefix and a suffix.  When the
    // argument is processed, a single argument should be generated consisting
    // of the prefix, the path of the flash, and the suffix, all joined by
    // spaces.
    FormattedPayload(String, String),

    // The filesystem path of the flash program configuration
    Config,
}

#[derive(Debug, Serialize)]
pub struct FlashConfig {
    program: FlashProgram,
    args: Vec<FlashArgument>,
}

impl FlashProgramConfig {
    fn new(path: Vec<&str>) -> Self {
        FlashProgramConfig::Path(path.iter().map(|f| f.to_string()).collect())
    }
}

impl FlashConfig {
    fn new(program: FlashProgram) -> Self {
        FlashConfig {
            program: program,
            args: vec![],
        }
    }

    //
    // Add a command-line argument to the flash program
    //
    fn arg<'a>(&'a mut self, val: &str) -> &'a mut Self {
        self.args.push(FlashArgument::Direct(val.to_string()));
        self
    }

    //
    // Add the path to the payload as an argument to the flash program
    //
    fn payload<'a>(&'a mut self) -> &'a mut Self {
        self.args.push(FlashArgument::Payload);
        self
    }

    //
    // Add a formatted payload as a single argument to the flash program.  The
    // argument will consists of the specified prefix, followed by the path to
    // the payload, followed by the specified suffix.
    //
    fn formatted_payload<'a>(
        &'a mut self,
        prefix: &str,
        suffix: &str,
    ) -> &'a mut Self {
        self.args.push(FlashArgument::FormattedPayload(
            prefix.to_string(),
            suffix.to_string(),
        ));
        self
    }

    //
    // Add a flasher configuration file as an argument to the flash program
    //
    fn config<'a>(&'a mut self) -> &'a mut Self {
        self.args.push(FlashArgument::Config);
        self
    }

    //
    // Slurp in any flash program configuration file and flatten it into
    // our overall configuration
    //
    pub fn flatten(&mut self) -> anyhow::Result<()> {
        if let FlashProgram::OpenOcd(ref config) = self.program {
            if let FlashProgramConfig::Path(ref path) = config {
                let p: PathBuf = path.iter().collect();
                let text = std::fs::read_to_string(p)?;
                self.program =
                    FlashProgram::OpenOcd(FlashProgramConfig::Payload(text));
            }
        }

        Ok(())
    }
}

pub fn config(board: &str) -> anyhow::Result<FlashConfig> {
    match board {
        "lpcxpresso55s69" | "gemini-bu-rot-1" | "gimlet-rot-1" => {
            let chip = if board == "lpcxpresso55s69" {
                "lpc55s69"
            } else {
                "lpc55s28"
            };

            let mut args = vec![];

            for arg in ["reset", "-t", chip].iter() {
                args.push(FlashArgument::Direct(arg.to_string()));
            }

            let mut flash = FlashConfig::new(FlashProgram::PyOcd(args));

            flash
                .arg("flash")
                .arg("-t")
                .arg(chip)
                .arg("--format")
                .arg("hex")
                .payload();

            Ok(flash)
        }
        "stm32f3-discovery" | "stm32f4-discovery" | "nucleo-h743zi2"
        | "nucleo-h753zi" | "stm32h7b3i-dk" | "gemini-bu-1" | "gimletlet-1"
        | "gimletlet-2" | "gimlet-1" | "psc-1" | "sidecar-1" | "stm32g031"
        | "stm32g070" | "stm32g0b1" => {
            let (dir, file) = if board == "stm32f3-discovery" {
                ("demo-stm32f4-discovery", "openocd-f3.cfg")
            } else if board == "stm32f4-discovery" {
                ("demo-stm32f4-discovery", "openocd.cfg")
            } else if board == "stm32g031" {
                ("demo-stm32g0-nucleo", "openocd.cfg")
            } else if board == "stm32g070" {
                ("demo-stm32g0-nucleo", "openocd.cfg")
            } else if board == "stm32g0b1" {
                ("demo-stm32g0-nucleo", "openocd.cfg")
            } else if board == "gemini-bu-1" {
                ("gemini-bu", "openocd.cfg")
            } else if board == "gimletlet-2" {
                ("gimletlet", "openocd.cfg")
            } else {
                ("demo-stm32h7-nucleo", "openocd.cfg")
            };

            let cfg = FlashProgramConfig::new(["app", dir, file].to_vec());

            let mut flash = FlashConfig::new(FlashProgram::OpenOcd(cfg));

            flash
                .arg("-f")
                .config()
                .arg("-c")
                .formatted_payload("program", "verify reset")
                .arg("-c")
                .arg("exit");

            Ok(flash)
        }
        _ => {
            anyhow::bail!("unrecognized board {}", board);
        }
    }
}

pub fn run(verbose: bool, cfg: &Path) -> anyhow::Result<()> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let toml = Config::from_file(&cfg)?;

    let mut out = PathBuf::from("target");
    out.push(toml.name);
    out.push("dist");

    let config = config(&toml.board.as_str())?;

    let (mut flash, reset) = match config.program {
        FlashProgram::PyOcd(ref reset_args) => {
            let mut flash = Command::new("pyocd");
            let mut reset = Command::new("pyocd");

            for arg in config.args {
                match arg {
                    FlashArgument::Direct(ref val) => {
                        flash.arg(val);
                    }
                    FlashArgument::Payload => {
                        flash.arg(out.join("final.ihex"));
                    }
                    _ => {
                        anyhow::bail!("unexpected pyOCD argument {:?}", arg);
                    }
                }
            }

            for arg in reset_args {
                if let FlashArgument::Direct(ref val) = arg {
                    reset.arg(val);
                } else {
                    anyhow::bail!("unexpected pyOCD reset argument {:?}", arg);
                }
            }

            if let Ok(id) = env::var("PYOCD_PROBE_ID") {
                flash.arg("-u");
                flash.arg(&id);
                reset.arg("-u");
                reset.arg(&id);
            }

            if verbose {
                flash.arg("-v");
                reset.arg("-v");
            }

            (flash, Some(reset))
        }

        FlashProgram::OpenOcd(conf) => {
            let mut flash = Command::new("openocd");

            // Note that OpenOCD only deals with slash paths, not native paths
            // (that is, its file arguments are forward-slashed delimited even
            // when/where the path separator is a back-slash) -- so whenever
            // dealing with a path that is an argument, we be sure to always
            // give it a slash path regardless of platform.
            let path = match conf {
                FlashProgramConfig::Path(ref path) => path,
                _ => {
                    anyhow::bail!("unexpected OpenOCD conf {:?}", conf);
                }
            };

            for arg in config.args {
                match arg {
                    FlashArgument::Direct(ref val) => {
                        flash.arg(val);
                    }
                    FlashArgument::FormattedPayload(ref pre, ref post) => {
                        flash.arg(format!(
                            "{} {} {}",
                            pre,
                            out.join("final.srec").to_slash().unwrap(),
                            post,
                        ));
                    }
                    FlashArgument::Config => {
                        flash.arg(path.join("/"));
                    }
                    _ => {
                        anyhow::bail!("unexpected OpenOCD argument {:?}", arg);
                    }
                }
            }

            (flash, None)
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
