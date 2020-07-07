#![feature(try_blocks)]

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

use structopt::StructOpt;

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

fn main() -> Result<(), Box<dyn Error>> {
    let xtask = Xtask::from_args();

    match xtask {
        Xtask::Dist { cfg } => {
            dist::package(cfg)?;
        }
        Xtask::Gdb { cfg, gdb_cfg } => {
            dist::package(cfg)?;
            gdb::run(gdb_cfg)?;
        }
        Xtask::Build => {
            let path = env::current_dir()?;
            let manifest_path = path.join("Cargo.toml");
            //let target = "thumbv8m.main-none-eabihf";
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
