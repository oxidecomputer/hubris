// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::Result;
use structopt::StructOpt;

use serde::Deserialize;

use indexmap::IndexMap;

mod check;
mod clippy;
mod dist;
mod elf;
mod flash;
mod gdb;
mod humility;
mod license;
mod task_slot;
mod test;

#[derive(Debug, StructOpt)]
#[structopt(
    max_term_width = 80,
    about = "extra tasks to help you work on Hubris"
)]
enum Xtask {
    /// Builds a collection of cross-compiled binaries at non-overlapping
    /// addresses, and then combines them into a system image with an
    /// application descriptor.
    Dist {
        /// Request verbosity from tools we shell out to.
        #[structopt(short)]
        verbose: bool,
        /// Run `cargo tree --edges features ...` before each invocation of
        /// `cargo rustc ...`
        #[structopt(short, long)]
        edges: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Builds one or more cross-compiled binary as it would appear in the
    /// output of `dist`, but without all the other binaries or the final build
    /// archive. This is useful for iterating on a single task.
    Build {
        /// Request verbosity from tools we shell out to.
        #[structopt(short)]
        verbose: bool,
        /// Run `cargo tree --edges features ...` before each invocation of
        /// `cargo rustc ...`
        #[structopt(short, long)]
        edges: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
        /// Name of task(s) to build.
        #[structopt(min_values = 1)]
        tasks: Vec<String>,
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
        /// the target to build for, uses [package.metadata.build.target] if not
        /// passed
        #[structopt(long)]
        target: Option<String>,

        /// the package you're trying to build, uses current directory if not
        /// passed
        #[structopt(short)]
        package: Option<String>,

        /// check all packages, not only one
        #[structopt(long)]
        all: bool,
    },

    /// Runs `cargo clippy` on a specified task
    Clippy {
        /// the target to build for, uses [package.metadata.build.target] if not
        /// passed
        #[structopt(long)]
        target: Option<String>,

        /// the package you're trying to build, uses current directory if not
        /// passed
        #[structopt(short)]
        package: Option<String>,

        /// check all packages, not only one
        #[structopt(long)]
        all: bool,
    },

    /// Show a task's .task_slot_table contents
    TaskSlots {
        /// Path to task executable
        task_bin: PathBuf,
    },

    /// Check that all .rs files have the MPL header
    LicenseCheck,
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
    stacksize: Option<u32>,
    bootloader: Option<Bootloader>,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
    #[serde(default)]
    config: Option<toml::Value>,
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
struct Bootloader {
    path: PathBuf,
    name: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    sections: IndexMap<String, String>,
    #[serde(default)]
    sharedsyms: Vec<String>,
    imagea_flash_start: u32,
    imagea_flash_size: u32,
    imagea_ram_start: u32,
    imagea_ram_size: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Kernel {
    path: PathBuf,
    name: String,
    requires: IndexMap<String, u32>,
    stacksize: Option<u32>,
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
    stacksize: Option<u32>,
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
    #[serde(default, deserialize_with = "deserialize_task_slot")]
    task_slots: IndexMap<String, String>,
    #[serde(default)]
    config: Option<toml::Value>,
}

/// In the common case, task slots map back to a task of the same name (e.g.
/// `gpio_driver`, `rcc_driver`).  However, certain tasks need generic task
/// slot names, e.g. they'll have a task slot named `spi_driver` which will
/// be mapped to a specific SPI driver task (`spi2_driver`).
///
/// This deserializer lets us handle both cases, while making the common case
/// easiest to write.  In `app.toml`, you can write something like
/// ```toml
/// task-slots = [
///     "gpio_driver",
///     "i2c_driver",
///     "rcc_driver",
///     {spi_driver: "spi2_driver"},
/// ]
/// ```
fn deserialize_task_slot<'de, D>(
    deserializer: D,
) -> Result<IndexMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Clone, Debug, Deserialize)]
    #[serde(untagged)]
    enum ArrayItem {
        Identity(String),
        Remap(IndexMap<String, String>),
    }
    let s: Vec<ArrayItem> = serde::Deserialize::deserialize(deserializer)?;
    let mut out = IndexMap::new();
    for a in s {
        match a {
            ArrayItem::Identity(s) => {
                out.insert(s.clone(), s.clone());
            }
            ArrayItem::Remap(m) => {
                if m.len() != 1 {
                    return Err(serde::de::Error::invalid_length(
                        m.len(),
                        &"a single key-value pair",
                    ));
                }
                let (k, v) = m.iter().next().unwrap();
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(out)
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

// For commands which may execute on specific packages, this enum
// identifies the set of packages that should be operated upon.
enum RequestedPackages {
    // Specifies a single specific (Package, Target) pair.
    Specific(Option<String>, Option<String>),
    // Specifies the command should operate on all packages.
    All,
}

impl RequestedPackages {
    fn new(package: Option<String>, target: Option<String>, all: bool) -> Self {
        if all {
            RequestedPackages::All
        } else {
            RequestedPackages::Specific(package, target)
        }
    }
}

// Runs a function on the a requested set of packages.
//
// # Arguments
//
// * `requested` - The requested packages to operate upon.
// * `func` - The function to execute for requested packages,
//            acting on a (Package, Target) pair.
fn run_for_packages<F>(requested: RequestedPackages, func: F) -> Result<()>
where
    F: Fn(Option<String>, Option<String>) -> Result<()>,
{
    match requested {
        RequestedPackages::Specific(package, target) => func(package, target)?,
        RequestedPackages::All => {
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

                func(Some(package.name), target)?;
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let xtask = Xtask::from_args();

    match xtask {
        Xtask::Dist {
            verbose,
            edges,
            cfg,
        } => {
            dist::package(verbose, edges, &cfg, None)?;
        }
        Xtask::Build {
            verbose,
            edges,
            cfg,
            tasks,
        } => {
            dist::package(verbose, edges, &cfg, Some(tasks))?;
        }
        Xtask::Flash { verbose, cfg } => {
            dist::package(verbose, false, &cfg, None)?;
            flash::run(verbose, &cfg)?;
        }
        Xtask::Gdb { cfg, gdb_cfg } => {
            dist::package(false, false, &cfg, None)?;
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
                dist::package(verbose, false, &cfg, None)?;
                flash::run(verbose, &cfg)?;
            }

            test::run(verbose, &cfg)?;
        }
        Xtask::Check {
            package,
            target,
            all,
        } => {
            let requested = RequestedPackages::new(package, target, all);
            run_for_packages(requested, check::run)?;
        }
        Xtask::Clippy {
            package,
            target,
            all,
        } => {
            let requested = RequestedPackages::new(package, target, all);
            run_for_packages(requested, clippy::run)?;
        }
        Xtask::TaskSlots { task_bin } => {
            task_slot::dump_task_slot_table(&task_bin)?;
        }
        Xtask::LicenseCheck => {
            if !license::check()? {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
