#![feature(try_blocks)]

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use structopt::StructOpt;

use indexmap::IndexMap;

mod build;
mod dist;
mod gdb;

#[derive(Debug, StructOpt)]
#[structopt(
    max_term_width = 80,
    about = "extra tasks to help you work on Hubris"
)]
enum Xtask {
    /// Builds a collection of cross-compiled binaries at non-overlapping addresses,
    /// and then combines them into a system image with an application descriptor.
    Dist {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Runs `xtask dist` and then runs a properly configured gdb for you.
    Gdb {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Path to the gdb configuation script.
        gdb_cfg: PathBuf,
    },

    /// builds a sub-project
    Build,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    name: String,
    target: String,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Kernel {
    path: PathBuf,
    name: String,
    requires: IndexMap<String, u32>,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Supervisor {
    notification: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Output {
    address: u32,
    size: u32,
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write: bool,
    #[serde(default)]
    execute: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Task {
    path: PathBuf,
    name: String,
    requires: IndexMap<String, u32>,
    priority: u32,
    #[serde(default)]
    uses: Vec<String>,
    #[serde(default)]
    start: bool,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    interrupts: IndexMap<String, u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Peripheral {
    address: u32,
    size: u32,
}

struct LoadSegment {
    source_file: PathBuf,
    data: Vec<u8>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let xtask = Xtask::from_args();

    match xtask {
        Xtask::Dist { cfg } => {
            dist::package(&cfg)?;
        }
        Xtask::Gdb { cfg, gdb_cfg } => {
            dist::package(&cfg)?;
            gdb::run(&cfg, &gdb_cfg)?;
        }
        Xtask::Build => {
            let path = env::current_dir()?;
            let manifest_path = path.join("Cargo.toml");
            let target = get_target(&manifest_path)?;

            build::run(&path, &target)?;
        }
    }

    Ok(())
}

fn get_target(manifest_path: &Path) -> Result<String, Box<dyn Error>> {
    let contents = std::fs::read(manifest_path)?;
    let toml: toml::Value = toml::from_slice(&contents)?;

    // we're on nightly, let's enjoy it
    let target = try {
        toml.get("package")?
            .get("metadata")?
            .get("build")?
            .get("target")?
            .as_str()?
    };

    match target {
        Some(target) => Ok(target.to_string()),
        None => Err(String::from("Could not find target, please set [package.metadata.build.target] in Cargo.toml").into()),
    }
}
