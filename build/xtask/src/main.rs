// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{bail, Result};
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
        #[clap(short, long, conflicts_with = "list")]
        edges: bool,
        /// Print a list of all tasks
        #[clap(short, long)]
        list: bool,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
        /// Name of task(s) to build.
        #[clap(min_values = 1, conflicts_with = "list")]
        tasks: Vec<String>,
    },

    /// Runs `xtask dist` and flashes the image onto an attached target
    Flash {
        #[clap(flatten)]
        args: HumilityArgs,
    },

    /// Runs `xtask dist`, `xtask flash` and then `humility gdb`
    Gdb {
        /// Do not flash a new image; just run `humility gdb`
        #[clap(long, short)]
        noflash: bool,

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

        /// Compare this to a previously saved file of sizes
        #[clap(long)]
        compare: bool,

        /// Write JSON out to a file?
        #[clap(long)]
        save: bool,
    },

    /// Runs `humility`, passing any arguments
    Humility {
        #[clap(flatten)]
        args: HumilityArgs,
    },

    /// Runs `xtask dist`, `xtask flash` and then `humility test`
    Test {
        /// Do not flash a new image; just run `humility test`
        #[clap(long, short)]
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

#[derive(Clone, Debug, Parser)]
pub struct HumilityArgs {
    /// Path to the image configuration file, in TOML.
    cfg: PathBuf,

    /// Image name to flash
    #[clap(long, default_value = "default")]
    image_name: String,

    /// Request verbosity from tools we shell out to.
    #[clap(short, long)]
    verbose: bool,

    /// Extra options to pass to Humility
    #[clap(last = true)]
    extra_options: Vec<String>,
}

fn main() -> Result<()> {
    // Check whether we're running from the right directory
    if let Ok(root_path) = std::env::var("CARGO_MANIFEST_DIR") {
        // This is $HUBRIS_DIR/build/xtask/, so we pop twice to get the Hubris
        // root directory, then compare against our working directory.
        let root_path = PathBuf::from(root_path);
        let hubris_dir = root_path.parent().unwrap().parent().unwrap();
        let current_dir = std::env::current_dir()?;
        if hubris_dir.canonicalize()? != current_dir.canonicalize()? {
            bail!(
                "`cargo xtask` must be run from root directory of Hubris repo"
            );
        }
    }

    let xtask = Xtask::parse();
    run(xtask)
}

fn run(xtask: Xtask) -> Result<()> {
    match xtask {
        Xtask::Dist {
            verbose,
            edges,
            cfg,
        } => {
            let allocs = dist::package(verbose, edges, &cfg, None)?;
            for (_, a) in allocs {
                sizes::run(&cfg, &a, true, false, false)?;
            }
        }
        Xtask::Build {
            verbose,
            edges,
            list,
            cfg,
            tasks,
        } => {
            if list {
                dist::list_tasks(&cfg)?;
            } else {
                dist::package(verbose, edges, &cfg, Some(tasks))?;
            }
        }
        Xtask::Flash { mut args } => {
            dist::package(args.verbose, false, &args.cfg, None)?;
            let toml = Config::from_file(&args.cfg)?;
            let chip = ["-c", crate::flash::chip_name(&toml.board)?];
            args.extra_options.push("--force".to_string());
            //for img in toml.image_names {
                humility::run(&args, &chip, Some("flash"), false, &args.image_name)?;
            //}
        }
        Xtask::Sizes {
            verbose,
            cfg,
            compare,
            save,
        } => {
            let allocs = dist::package(verbose, false, &cfg, None)?;
            for (_, a) in allocs {
                sizes::run(&cfg, &a, false, compare, save)?;
            }
        }
        Xtask::Humility { args } => {
            humility::run(&args, &[], None, true, &args.image_name)?;
        }
        Xtask::Gdb { noflash, mut args } => {
            if !noflash {
                dist::package(args.verbose, false, &args.cfg, None)?;
                // Delegate flashing to `humility gdb`, which also modifies
                // the GDB startup script slightly (adding `stepi`)
                args.extra_options.push("--load".to_string());
            }
            humility::run(&args, &[], Some("gdb"), true, &args.image_name)?;
        }
        Xtask::Test { args, noflash } => {
            if !noflash {
                run(Xtask::Flash { args: args.clone() })?;
            }
            humility::run(&args, &[], Some("test"), false, &args.image_name)?;
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
