// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Building tasks for the host, to run under a test fixture.
//!
//! A task built this way gets the same features and `HUBRIS_*` environment as
//! it would in `dist` for the given app, so its build script sees the same
//! configuration (task slots, notifications, `[config]` sections). It is then
//! compiled for the host with userlib's host syscall implementation, and can
//! be run by a fixture such as `host-fixture`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::dist::PackageConfig;

pub struct HostBuildFlags {
    pub verbose: bool,
    pub release: bool,
}

/// Target directory for host builds, kept apart from the cross builds.
const TARGET_DIR: &str = "target/host";

/// Builds each named task of `app_toml` for the host, returning the path of
/// each resulting executable.
pub fn build(
    app_toml: &Path,
    tasks: &[String],
    flags: HostBuildFlags,
) -> Result<Vec<PathBuf>> {
    let cfg = PackageConfig::new(app_toml, flags.verbose, false)?;
    let profile = if flags.release { "release" } else { "debug" };

    let mut outputs = Vec::new();
    for task in tasks {
        let build_config =
            cfg.task_build_config(task).map_err(|e| anyhow!(e))?;

        // `BuildConfig::cmd` would pass the app's cross target; build the
        // command by hand so the host (no `--target`) is used instead.
        let mut cmd = std::process::Command::new(
            build_config.sysroot.join("bin").join("cargo"),
        );
        cmd.arg("rustc").arg("-p").arg(&build_config.crate_name);
        let mut args = build_config.args.iter();
        while let Some(arg) = args.next() {
            if arg == "--target" {
                args.next();
                continue;
            }
            cmd.arg(arg);
        }
        for (k, v) in &build_config.env {
            cmd.env(k, v);
        }
        cmd.arg("--target-dir").arg(TARGET_DIR);
        if flags.release {
            cmd.arg("--release");
        }
        // As in `dist`: feature combinations that produce duplicate
        // attributes are a bug, not a warning.
        cmd.arg("--").arg("-Dunused_attributes");

        println!("building {task} ({}) for the host", build_config.crate_name);
        let status = cmd
            .status()
            .with_context(|| format!("running cargo for {task}"))?;
        if !status.success() {
            bail!("host build of {task} failed");
        }

        let exe = Path::new(TARGET_DIR)
            .join(profile)
            .join(&build_config.crate_name);
        if !exe.exists() {
            bail!("expected {} to exist after building {task}", exe.display());
        }
        outputs.push(exe);
    }
    Ok(outputs)
}
