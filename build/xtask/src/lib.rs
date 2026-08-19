// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This library crate exists to make it possible to run `tests/`.

pub mod auxflash;
pub mod caboose_pos;
pub mod config;
pub mod dist;
pub mod flash;
pub mod gha_prepare_artifacts;
pub mod graph;
pub mod humility;
pub mod i2c_codegen;
pub mod lsp;
pub mod passthrough;
pub mod print;
pub mod rust_analyzer;
pub mod sizes;
pub mod task_slot;

pub use crate::config::Config;
use clap::Parser;
use std::path::PathBuf;

#[derive(Clone, Debug, Parser)]
pub struct HumilityArgs {
    /// Path to the image configuration file, in TOML.
    pub cfg: PathBuf,

    /// Image name to flash
    #[clap(long)]
    pub image_name: Option<String>,

    /// Request verbosity from tools we shell out to.
    #[clap(short, long)]
    pub verbose: bool,

    /// Extra options to pass to Humility
    #[clap(last = true)]
    pub extra_options: Vec<String>,
}

#[derive(Clone, Debug, Parser, Default)]
pub struct CabooseArgs {
    /// Overrides the `VERS` string in the caboose.
    ///
    /// This is intended to be used when an engineering image must be
    /// flashed in an environment that expects a particular caboose version.
    ///
    /// This environment variable is, naturally, ignored if the app.toml does
    /// not have a [caboose] section.
    #[clap(env = "HUBRIS_CABOOSE_VERS")]
    pub version_override: Option<String>,
}
