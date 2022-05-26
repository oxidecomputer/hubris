// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use crate::config::Config;

mod clippy;
mod config;
mod dist;
mod elf;
mod flash;
mod humility;
mod sizes;
mod task_slot;

#[derive(Debug, Parser)]
#[clap(max_term_width = 80, about = "extra tasks to help you work on Hubris")]
enum Xtask {
    /// Builds a collection of cross-compiled binaries at non-overlapping
    /// addresses, and then combines them into a system image with an
    /// application descriptor.
    Dist {
        /// Request verbosity from tools we shell out to.
        #[clap(short)]
        verbose: bool,
        /// Run `cargo tree --edges features ...` before each invocation of
        /// `cargo rustc ...`
        #[clap(short, long)]
        edges: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Builds one or more cross-compiled binary as it would appear in the
    /// output of `dist`, but without all the other binaries or the final build
    /// archive. This is useful for iterating on a single task.
    Build {
        /// Request verbosity from tools we shell out to.
        #[clap(short)]
        verbose: bool,
        /// Run `cargo tree --edges features ...` before each invocation of
        /// `cargo rustc ...`
        #[clap(short, long)]
        edges: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
        /// Name of task(s) to build.
        #[clap(min_values = 1)]
        tasks: Vec<String>,
    },

    /// Runs `xtask dist` and flashes the image onto an attached target
    Flash {
        #[clap(flatten)]
        args: HumilityArgs,
    },

    /// Runs `xtask dist` and reports the sizes of resulting tasks
    Sizes {
        /// Request verbosity from tools we shell out to.
        #[clap(short)]
        verbose: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Runs `humility`, passing any arguments
    Humility {
        #[clap(flatten)]
        args: HumilityArgs,
    },

    /// Runs `xtask dist`, `xtask flash` and then `humility test`
    Test {
        /// Do not flash a new image; just run `humility test`
        #[clap(short)]
        noflash: bool,

        #[clap(flatten)]
        args: HumilityArgs,
    },

    /// Runs `cargo clippy` on a specified task
    Clippy {
        /// Request verbosity from tools we shell out to.
        #[clap(short)]
        verbose: bool,

        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Name of task(s) to check.
        #[clap(min_values = 1)]
        tasks: Vec<String>,

        /// Extra options to pass to clippy
        #[clap(last = true)]
        extra_options: Vec<String>,
    },

    /// Show a task's .task_slot_table contents
    TaskSlots {
        /// Path to task executable
        task_bin: PathBuf,
    },
}

#[derive(Debug, Parser)]
pub struct HumilityArgs {
    /// Path to the image configuration file, in TOML.
    cfg: PathBuf,

    /// Request verbosity from tools we shell out to.
    #[clap(short, long)]
    verbose: bool,

    /// Extra options to pass to clippy
    #[clap(last = true)]
    extra_options: Vec<String>,
}

// For commands which may execute on specific packages, this enum
// identifies the set of packages that should be operated upon.
fn main() -> Result<()> {
    let xtask = Xtask::parse();

    match xtask {
        Xtask::Dist {
            verbose,
            edges,
            cfg,
        } => {
            dist::package(verbose, edges, &cfg, None)?;
            sizes::run(&cfg, true)?;
        }
        Xtask::Build {
            verbose,
            edges,
            cfg,
            tasks,
        } => {
            dist::package(verbose, edges, &cfg, Some(tasks))?;
        }
        Xtask::Flash { args } => {
            dist::package(args.verbose, false, &args.cfg, None)?;
            let toml = Config::from_file(&args.cfg)?;
            let chip = ["-c", crate::flash::chip_name(&toml.board)?];
            humility::run(&args, &chip, Some("flash"))?;
        }
        Xtask::Sizes { verbose, cfg } => {
            dist::package(verbose, false, &cfg, None)?;
            sizes::run(&cfg, false)?;
        }
        Xtask::Humility { args } => {
            humility::run(&args, &[], None)?;
        }
        Xtask::Test { args, noflash } => {
            if !noflash {
                dist::package(args.verbose, false, &args.cfg, None)?;
                let toml = Config::from_file(&args.cfg)?;
                let chip = ["-c", crate::flash::chip_name(&toml.board)?];
                humility::run(&args, &chip, Some("flash"))?;
            }
            humility::run(&args, &[], Some("test"))?;
        }
        Xtask::Clippy {
            verbose,
            cfg,
            tasks,
            extra_options,
        } => {
            clippy::run(verbose, cfg, &tasks, &extra_options)?;
        }
        Xtask::TaskSlots { task_bin } => {
            task_slot::dump_task_slot_table(&task_bin)?;
        }
    }

    Ok(())
}
