// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::config::Config;

pub fn run(
    verbose: bool,
    cfg: PathBuf,
    tasks: &[String],
    options: &[String],
) -> Result<()> {
    let toml = Config::from_file(&cfg)?;

    let mut src_dir = cfg;
    src_dir.pop();
    let src_dir = src_dir;

    if tasks.is_empty() {
        bail!("Must provide one or more task names");
    }

    for name in tasks {
        if !toml.tasks.contains_key(name) && name != "kernel" {
            bail!("{}", toml.task_name_suggestion(name));
        }
    }

    for (i, name) in tasks.iter().enumerate() {
        let (task_name, path) = if name == "kernel" {
            ("kernel", &toml.kernel.path)
        } else {
            let task_toml = &toml.tasks[name];
            (task_toml.name.as_str(), &task_toml.path)
        };
        if tasks.len() > 1 {
            if i > 0 {
                println!();
            }
            println!(
                "================== {} [{}] ==================",
                name, task_name
            );
        }

        let build_config = if name == "kernel" {
            // Allocate memories, using the real code since it's easier tha
            // building a realistic mock
            let mut memories = toml.memories()?;
            let allocs = crate::dist::allocate_all(
                &toml.kernel,
                &toml.tasks,
                &mut memories,
            )?;
            // Pick dummy entry points for each task
            let entry_points = allocs
                .tasks
                .iter()
                .map(|(k, v)| (k.clone(), v["flash"].start))
                .collect();

            let kconfig = crate::dist::make_kconfig(
                &toml.target,
                &toml.tasks,
                &toml.peripherals,
                &allocs.tasks,
                toml.stacksize,
                &toml.outputs,
                &entry_points,
                &toml.extratext,
            )?;
            println!("Got kconfig");
            let kconfig = ron::ser::to_string(&kconfig)?;

            println!("serialized");
            toml.kernel_build_config(
                verbose,
                &[
                    ("HUBRIS_KCONFIG", &kconfig),
                    ("HUBRIS_IMAGE_ID", "1234"), // dummy image ID
                ],
            )
        } else {
            toml.task_build_config(name, verbose).unwrap()
        };
        println!("bla");
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

        cmd.current_dir(&src_dir.join(&path));
        let status = cmd.status()?;
        if !status.success() {
            bail!("`cargo clippy` failed, see output for details");
        }
    }
    Ok(())
}
