// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Result};
use indexmap::IndexMap;
use std::path::PathBuf;

use crate::config::{Config, Name};

pub fn run(
    verbose: bool,
    cfg: PathBuf,
    tasks: &[String],
    options: &[String],
) -> Result<()> {
    let toml = Config::from_file(&cfg)?;

    // If no arguments are passed in, run on every task and the kernel
    let tasks: Vec<Name> = if tasks.is_empty() {
        toml.tasks
            .keys()
            .map(|s| Name::Task(s.as_str()))
            .chain(std::iter::once(Name::Kernel))
            .collect()
    } else {
        tasks.iter().map(|s| Name::from(s.as_str())).collect()
    };

    for (i, name) in tasks.iter().enumerate() {
        let crate_name = toml.crate_name(*name)?;
        if tasks.len() > 1 {
            if i > 0 {
                println!();
            }
            println!(
                "================== {} [{}] ==================",
                name, crate_name
            );
        }

        let build_config = match name {
            Name::Kernel => {
                // Build dummy allocations for each task
                let fake_sizes: IndexMap<_, _> =
                    [("flash", 64), ("ram", 64)].into_iter().collect();
                let task_sizes = toml
                    .tasks
                    .keys()
                    .map(|name| (Name::Task(name.as_str()), fake_sizes.clone()))
                    .collect();

                let (allocs, _) =
                    crate::dist::allocate_all(&toml, &task_sizes)?;
                // Pick dummy entry points for each task
                let entry_points = allocs
                    .tasks
                    .iter()
                    .map(|(k, v)| (k.clone(), v["flash"].start))
                    .collect();

                let kconfig = crate::dist::make_kconfig(
                    &toml,
                    &allocs.tasks,
                    &entry_points,
                )?;
                let kconfig = ron::ser::to_string(&kconfig)?;

                toml.kernel_build_config(
                    verbose,
                    &[
                        ("HUBRIS_KCONFIG", &kconfig),
                        ("HUBRIS_IMAGE_ID", "1234"), // dummy image ID
                    ],
                    None,
                )
            }
            Name::Task(t) => toml.task_build_config(t, verbose, None).unwrap(),
        };
        let mut cmd = build_config.cmd("clippy");

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

        let status = cmd.status()?;
        if !status.success() {
            bail!("`cargo clippy` failed, see output for details");
        }
    }
    Ok(())
}
