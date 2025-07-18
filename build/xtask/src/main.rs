// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// siiiiigh this was supposed to be on globally, but missed
// applying to xtask itself -- so we have a lot of elided
// lifetimes. TODO turn this back on!
#![allow(elided_lifetimes_in_paths)]

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;

use crate::config::Config;

mod auxflash;
mod caboose_pos;
mod clippy;
mod config;
mod dist;
mod elf;
mod flash;
mod graph;
mod humility;
mod lsp;
mod print;
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
        /// Allow operation in a dirty checkout, i.e. don't clean before
        /// rebuilding even if it looks like we need to.
        #[clap(long)]
        dirty: bool,
        /// Configures the caboose for the generated archive.
        #[clap(flatten)]
        caboose_args: CabooseArgs,
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
        /// Allow operation in a dirty checkout, i.e. don't clean before
        /// rebuilding even if it looks like we need to.
        #[clap(long)]
        dirty: bool,
    },

    /// Runs `xtask dist` and flashes the image onto an attached target
    Flash {
        #[clap(flatten)]
        args: HumilityArgs,
        /// Allow operation in a dirty checkout, i.e. don't clean before
        /// rebuilding even if it looks like we need to.
        #[clap(long)]
        dirty: bool,
        /// Configures the caboose for the generated archive.
        #[clap(flatten)]
        caboose_args: CabooseArgs,
    },

    /// Runs `xtask dist`, `xtask flash` and then `humility gdb`
    Gdb {
        /// Do not flash a new image; just run `humility gdb`
        #[clap(long, short)]
        noflash: bool,

        #[clap(flatten)]
        args: HumilityArgs,

        /// Configures the caboose for the generated archive.
        #[clap(flatten)]
        caboose_args: CabooseArgs,
    },

    /// Runs `xtask dist` and reports the sizes of resulting tasks
    Sizes {
        /// `-v` shows chunk sizes; `-vv` makes build verbose
        #[clap(short, action = clap::ArgAction::Count)]
        verbose: u8,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Compare this to a previously saved file of sizes
        #[clap(long)]
        compare: bool,

        /// Write JSON out to a file?
        #[clap(long)]
        save: bool,
        /// Allow operation in a dirty checkout, i.e. don't clean before
        /// rebuilding even if it looks like we need to.
        #[clap(long)]
        dirty: bool,

        /// Configures the caboose for the generated archive.
        #[clap(flatten)]
        caboose_args: CabooseArgs,
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

        /// Configures the caboose for the generated archive.
        #[clap(flatten)]
        caboose_args: CabooseArgs,
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

    /// Generate a graph of task_slot dependencies ordered by priority.
    ///
    /// Priority inversions are denoted by thick red arrows.
    /// Normal task_slot dependencies are thin green arrows.
    /// Example:
    ///
    ///   cargo xtask graph -o app.dot $APP_TOML;
    ///   dot -Tsvg app.dot > app.svg;
    ///   xdg-open app.xvg
    Graph {
        /// Output file for Graphviz dot syntax graph.
        #[clap(short, long)]
        output: PathBuf,
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,
    },

    /// Print out information related to the build.
    ///
    /// Currently only useful to print the archive path, but may grow over time.
    Print {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Print the path to the archive
        #[clap(long)]
        archive: bool,

        /// If there are multiple possible images, print this one
        #[clap(long)]
        image_name: Option<String>,

        /// Print the expanded configuration
        #[clap(long)]
        expanded_config: bool,
    },

    /// Print a JSON blob with configuration info for `rust-analyzer`
    Lsp {
        /// Existing LSP clients.
        ///
        /// These should be JSON-encoded strings which can be parsed into an
        /// `LspClient`.
        #[clap(short, value_parser)]
        clients: Vec<lsp::LspClient>,

        /// Path to a Rust source file
        file: PathBuf,
    },
}

#[derive(Clone, Debug, Parser)]
pub struct HumilityArgs {
    /// Path to the image configuration file, in TOML.
    cfg: PathBuf,

    /// Image name to flash
    #[clap(long)]
    image_name: Option<String>,

    /// Request verbosity from tools we shell out to.
    #[clap(short, long)]
    verbose: bool,

    /// Extra options to pass to Humility
    #[clap(last = true)]
    extra_options: Vec<String>,
}

#[derive(Clone, Debug, Parser, Default)]
struct CabooseArgs {
    /// Overrides the `VERS` string in the caboose.
    ///
    /// This is intended to be used when an engineering image must be
    /// flashed in an environment that expects a particular caboose version.
    ///
    /// This environment variable is, naturally, ignored if the app.toml does
    /// not have a [caboose] section.
    #[clap(env = "HUBRIS_CABOOSE_VERS")]
    version_override: Option<String>,
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
            dirty,
            caboose_args,
        } => {
            let allocs =
                dist::package(verbose, edges, &cfg, None, dirty, caboose_args)?;
            for (_, (a, _)) in allocs {
                sizes::run(&cfg, &a, true, false, false, false)?;
            }
        }
        Xtask::Build {
            verbose,
            edges,
            list,
            cfg,
            tasks,
            dirty,
        } => {
            if list {
                dist::list_tasks(&cfg)?;
            } else {
                dist::package(
                    verbose,
                    edges,
                    &cfg,
                    Some(tasks),
                    dirty,
                    CabooseArgs::default(),
                )?;
            }
        }
        Xtask::Flash {
            dirty,
            mut args,
            caboose_args,
        } => {
            dist::package(
                args.verbose,
                false,
                &args.cfg,
                None,
                dirty,
                caboose_args,
            )?;
            let toml = Config::from_file(&args.cfg)?;
            let chipname =
                crate::flash::chip_name(&toml.board)?.ok_or_else(|| {
                    anyhow!(
                        "can't flash board: chip name missing \
                         from boards/{}.toml",
                        toml.board,
                    )
                })?;
            let chip = ["-c", &chipname];
            args.extra_options.push("--force".to_string());

            let image_name = if let Some(ref name) = args.image_name {
                if !toml.check_image_name(name) {
                    bail!("Image name {} not declared in TOML", name);
                }
                name
            } else {
                &toml.image_names[0]
            };

            humility::run(&args, &chip, Some("flash"), false, image_name)?;
        }
        Xtask::Sizes {
            verbose,
            cfg,
            compare,
            save,
            dirty,
            caboose_args,
        } => {
            let allocs = dist::package(
                verbose >= 2,
                false,
                &cfg,
                None,
                dirty,
                caboose_args,
            )?;
            for (_, (a, _)) in allocs {
                sizes::run(&cfg, &a, false, compare, save, verbose >= 1)?;
            }
        }
        Xtask::Humility { args } => {
            let toml = Config::from_file(&args.cfg)?;
            let image_name = if let Some(ref name) = args.image_name {
                if !toml.check_image_name(name) {
                    bail!("Image name {} not declared in TOML", name);
                }
                name
            } else {
                &toml.image_names[0]
            };
            humility::run(&args, &[], None, true, image_name)?;
        }
        Xtask::Gdb {
            noflash,
            mut args,
            caboose_args,
        } => {
            let toml = Config::from_file(&args.cfg)?;
            let image_name = if let Some(ref name) = args.image_name {
                if !toml.check_image_name(name) {
                    bail!("Image name {} not declared in TOML", name);
                }
                name
            } else {
                &toml.image_names[0]
            };
            if !noflash {
                dist::package(
                    args.verbose,
                    false,
                    &args.cfg,
                    None,
                    false,
                    caboose_args,
                )?;
                // Delegate flashing to `humility gdb`, which also modifies
                // the GDB startup script slightly (adding `stepi`)
                args.extra_options.push("--load".to_string());
            }
            humility::run(&args, &[], Some("gdb"), true, image_name)?;
        }
        Xtask::Test {
            args,
            noflash,
            caboose_args,
        } => {
            let toml = Config::from_file(&args.cfg)?;
            let image_name = if let Some(ref name) = args.image_name {
                if !toml.check_image_name(name) {
                    bail!("Image name {} not declared in TOML", name);
                }
                name
            } else {
                &toml.image_names[0]
            };
            if !noflash {
                run(Xtask::Flash {
                    args: args.clone(),
                    dirty: false,
                    caboose_args,
                })?;
            }
            humility::run(&args, &[], Some("test"), false, image_name)?;
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
        Xtask::Graph { output, cfg } => {
            graph::task_graph(&cfg, &output)?;
        }
        Xtask::Print {
            cfg,
            archive,
            image_name,
            expanded_config,
        } => {
            print::run(&cfg, archive, image_name, expanded_config)
                .context("could not print information about the build")?;
        }
        Xtask::Lsp { clients, file } => {
            lsp::run(&file, &clients)?;
        }
    }

    Ok(())
}
