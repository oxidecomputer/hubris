use std::path::PathBuf;

use anyhow::Result;
use structopt::StructOpt;

use serde::Deserialize;

use indexmap::IndexMap;

mod check;
mod dist;
mod flash;
mod gdb;
mod humility;
mod test;

#[derive(Debug, StructOpt)]
#[structopt(
    max_term_width = 80,
    about = "extra tasks to help you work on Hubris"
)]
enum Xtask {
    /// Builds a collection of cross-compiled binaries at non-overlapping addresses,
    /// and then combines them into a system image with an application descriptor.
    Dist {
        /// Request verbosity from tools we shell out to.
        #[structopt(short)]
        verbose: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Runs `xtask dist` and flashes the image onto an attached target
    Flash {
        /// Request verbosity from tools we shell out to.
        #[structopt(short)]
        verbose: bool,
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

    /// Runs `humility`, passing any arguments
    Humility {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Options to pass to Humility
        options: Vec<String>,
    },

    /// Runs `xtask dist`, `xtask flash` and then `humility test`
    Test {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Do not flash a new image; just run `humility test`
        #[structopt(short)]
        noflash: bool,

        /// Request verbosity from tools we shell out to.
        #[structopt(short)]
        verbose: bool,
    },

    /// Runs `cargo check` on a specific task
    Check {
        /// the target to build for, uses [package.metadata.build.target] if not passed
        #[structopt(long)]
        target: Option<String>,

        /// the package you're trying to build, uses current directory if not passed
        #[structopt(short)]
        package: Option<String>,

        /// check all packages, not only one
        #[structopt(long)]
        all: bool,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    name: String,
    target: String,
    board: String,
    #[serde(default)]
    signing: IndexMap<String, Signing>,
    secure: Option<bool>,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Signing {
    method: String,
    priv_key: Option<PathBuf>,
    root_cert: Option<PathBuf>,
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
    #[serde(default)]
    dma: bool,
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
    #[serde(default)]
    sections: IndexMap<String, String>,
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

fn main() -> Result<()> {
    let xtask = Xtask::from_args();

    match xtask {
        Xtask::Dist { verbose, cfg } => {
            dist::package(verbose, &cfg)?;
        }
        Xtask::Flash { verbose, cfg } => {
            dist::package(verbose, &cfg)?;
            flash::run(verbose, &cfg)?;
        }
        Xtask::Gdb { cfg, gdb_cfg } => {
            dist::package(false, &cfg)?;
            gdb::run(&cfg, &gdb_cfg)?;
        }
        Xtask::Humility { cfg, options } => {
            humility::run(&cfg, &options)?;
        }
        Xtask::Test {
            cfg,
            noflash,
            verbose,
        } => {
            if !noflash {
                dist::package(verbose, &cfg)?;
                flash::run(verbose, &cfg)?;
            }

            test::run(verbose, &cfg)?;
        }
        Xtask::Check {
            package,
            target,
            all,
        } => {
            if !all {
                check::run(package, target)?;
            } else {
                use cargo_metadata::MetadataCommand;

                let metadata = MetadataCommand::new()
                    .manifest_path("./Cargo.toml")
                    .exec()
                    .unwrap();

                #[derive(Debug, Deserialize)]
                struct CustomMetadata {
                    build: Option<BuildMetadata>,
                }

                #[derive(Debug, Deserialize)]
                struct BuildMetadata {
                    target: Option<String>,
                }

                for id in &metadata.workspace_members {
                    let package = metadata
                        .packages
                        .iter()
                        .find(|p| &p.id == id)
                        .unwrap()
                        .clone();

                    let m: Option<CustomMetadata> =
                        serde_json::from_value(package.metadata)?;

                    let target = (|| m?.build?.target)();

                    check::run(Some(package.name), target)?;
                }
            }
        }
    }

    Ok(())
}
