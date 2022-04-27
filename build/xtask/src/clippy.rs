// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};

use crate::{dist::BuildConfig, Config};

pub fn run(
    verbose: bool,
    cfg: PathBuf,
    tasks: &[String],
    options: &[String],
) -> Result<()> {
    let toml = Config::from_file(&cfg)?;
    let task_names = toml.tasks.keys().cloned().collect::<Vec<_>>().join(",");
    let shared_syms: Option<&[String]> = None;

    let mut src_dir = cfg.to_path_buf();
    src_dir.pop();
    let src_dir = src_dir;

    if tasks.is_empty() {
        bail!("Must provide one or more task names");
    }

    for name in tasks {
        if !toml.tasks.contains_key(name) {
            bail!("{}", toml.task_name_suggestion(name));
        }
    }

    for (i, name) in tasks.iter().enumerate() {
        let task_toml = &toml.tasks[name];
        if tasks.len() > 1 {
            if i > 0 {
                println!();
            }
            println!(
                "================== {} [{}] ==================",
                name, task_toml.name
            );
        }

        let build_config = BuildConfig::new(
            &cfg,
            &toml.target,
            &toml.board,
            verbose,
            Some(&task_toml.name),
            &task_toml.features,
            &task_names,
            &toml.secure_separation,
            &shared_syms,
            &task_toml.config,
            &toml.config,
            &[],
        );
        let mut cmd = Command::new("cargo");
        cmd.arg("clippy");
        build_config.apply(&mut cmd);

        cmd.arg("--");
        cmd.arg("-W");
        cmd.arg("clippy::all");
        cmd.arg("-A");
        cmd.arg("clippy::missing_safety_doc");
        cmd.arg("-A");
        cmd.arg("clippy::identity_op");
        cmd.arg("-A");
        cmd.arg("clippy::too_many_arguments");

        for opt in options {
            cmd.arg(opt);
        }

        cmd.current_dir(&src_dir.join(&task_toml.path));
        let status = cmd.status()?;
        if !status.success() {
            bail!("`cargo clippy` failed, see output for details");
        }
    }
    Ok(())
}
