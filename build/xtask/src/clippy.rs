// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{bail, Result};
use indexmap::IndexMap;

use crate::config::Config;

pub fn run(
    verbose: bool,
    cfg: PathBuf,
    tasks: &[String],
    options: &[String],
) -> Result<()> {
    let toml = Config::from_file(&cfg)?;

    let mut tasks = tasks.to_vec();

    if tasks.is_empty() {
        tasks.extend(toml.tasks.keys().cloned());
        tasks.push("kernel".to_string());
    }

    for name in &tasks {
        if !toml.tasks.contains_key(name) && name != "kernel" {
            bail!("{}", toml.task_name_suggestion(name));
        }
    }

    for (i, name) in tasks.iter().enumerate() {
        let crate_name = if name == "kernel" {
            "kernel"
        } else {
            let task_toml = &toml.tasks[name];
            task_toml.name.as_str()
        };
        if tasks.len() > 1 {
            if i > 0 {
                println!();
            }
            println!(
                "================== {} [{}] ==================",
                name, crate_name
            );
        }

        let build_config = if name == "kernel" {
            // Build dummy allocations for each task
            let fake_sizes: IndexMap<_, _> =
                [("flash", 64), ("ram", 64)].into_iter().collect();
            let task_sizes = toml
                .tasks
                .keys()
                .map(|name| (name.as_str(), fake_sizes.clone()))
                .collect();

            let allocated = crate::dist::allocate_all(
                &toml,
                &task_sizes,
                toml.caboose.as_ref(),
            )?;

            let (allocs, _) = allocated
                .get(&toml.image_names[0])
                .ok_or_else(|| anyhow::anyhow!("Failed to get image name"))?;

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
                &toml.image_names[0],
                &None,
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
        } else {
            toml.task_build_config(name, verbose, None).unwrap()
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
        cmd.arg("-W");
        cmd.arg("elided_lifetimes_in_paths");

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
