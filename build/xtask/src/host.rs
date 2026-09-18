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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use crate::config::BuildConfig;
use crate::dist::PackageConfig;

pub struct HostBuildFlags {
    pub verbose: bool,
    pub release: bool,
}

/// Target directory for host builds, kept apart from the cross builds.
const TARGET_DIR: &str = "target/host";

/// Builds one crate of the app for the host, from the build configuration
/// `dist` would use for it, returning the executable's path.
fn build_crate(
    build_config: &BuildConfig<'_>,
    flags: &HostBuildFlags,
) -> Result<PathBuf> {
    let profile = if flags.release { "release" } else { "debug" };

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

    println!("building {} for the host", build_config.crate_name);
    let status = cmd.status().with_context(|| {
        format!("running cargo for {}", build_config.crate_name)
    })?;
    if !status.success() {
        bail!("host build of {} failed", build_config.crate_name);
    }

    let exe = Path::new(TARGET_DIR)
        .join(profile)
        .join(&build_config.crate_name);
    if !exe.exists() {
        bail!(
            "expected {} to exist after building {}",
            exe.display(),
            build_config.crate_name
        );
    }
    Ok(exe)
}

/// Builds each named task of `app_toml` for the host, returning the path of
/// each resulting executable.
pub fn build(
    app_toml: &Path,
    tasks: &[String],
    flags: HostBuildFlags,
) -> Result<Vec<PathBuf>> {
    let cfg = PackageConfig::new(app_toml, flags.verbose, false)?;
    tasks
        .iter()
        .map(|task| {
            let build_config =
                cfg.task_build_config(task).map_err(|e| anyhow!(e))?;
            build_crate(&build_config, &flags)
        })
        .collect()
}

/// The task named this in the manifest is the idle task: it is never built
/// or run on the host, where the kernel advances virtual time instead of
/// running it.
const IDLE_TASK: &str = "idle";

/// What `cargo xtask host-run` writes for the kernel; see `HostConfig` in
/// `kern::arch::host`, which this must match field for field.
#[derive(Serialize)]
struct HostConfig {
    tasks: Vec<HostTask>,
    stop_at: Option<u64>,
}

#[derive(Serialize)]
struct HostTask {
    name: String,
    executable: Option<String>,
    slots: BTreeMap<String, u16>,
    idle: bool,
    interface: Option<String>,
}

pub struct HostRunFlags {
    pub build: HostBuildFlags,
    /// Value for `HUBRIS_HOST_TRACE`: `all`, or the tasks to trace.
    pub trace: Option<String>,
    /// Stop when virtual time would pass this tick.
    pub stop_at: Option<u64>,
    /// Build everything but don't launch the kernel.
    pub no_run: bool,
}

/// Builds every task and the kernel of `app_toml` for the host, writes the
/// kernel's run configuration, and launches the kernel.
pub fn run(app_toml: &Path, flags: HostRunFlags) -> Result<()> {
    let cfg = PackageConfig::new(app_toml, flags.build.verbose, false)?;
    let toml = &cfg.toml;

    // Task indices follow manifest order, as in `dist`.
    let index_of = |name: &str| -> Result<u16> {
        toml.tasks
            .get_index_of(name)
            .map(|i| i as u16)
            .ok_or_else(|| anyhow!("task slot refers to unknown task {name}"))
    };

    let mut host_tasks = Vec::new();
    let mut kconfig_tasks = Vec::new();
    for (name, task) in &toml.tasks {
        let idle = name == IDLE_TASK;
        let executable = if idle {
            None
        } else {
            let build_config =
                cfg.task_build_config(name).map_err(|e| anyhow!(e))?;
            let exe = build_crate(&build_config, &flags.build)?;
            Some(dunce::canonicalize(&exe)?.display().to_string())
        };
        let slots = task
            .task_slots
            .iter()
            .map(|(slot, target)| Ok((slot.clone(), index_of(target)?)))
            .collect::<Result<_>>()?;
        // By convention a task named `foo` serves `idl/foo.idol`, when that
        // exists; the kernel uses it to decode traffic in its trace.
        let idol = Path::new("idl").join(format!("{name}.idol"));
        let interface = idol
            .exists()
            .then(|| {
                dunce::canonicalize(&idol).map(|p| p.display().to_string())
            })
            .transpose()?;
        host_tasks.push(HostTask {
            name: name.clone(),
            executable,
            slots,
            idle,
            interface,
        });
        // Memory placement is meaningless on the host; the kernel's build
        // script ignores these regions and gives every task the whole
        // address space.
        let placeholder = build_kconfig::OwnedAddress {
            region_name: "host".to_string(),
            offset: 0,
        };
        kconfig_tasks.push(build_kconfig::TaskConfig {
            owned_regions: BTreeMap::new(),
            shared_regions: BTreeSet::new(),
            entry_point: placeholder.clone(),
            initial_stack: placeholder,
            priority: task.priority,
            start_at_boot: task.start,
        });
    }

    let kconfig = build_kconfig::KernelConfig {
        features: toml.kernel.features.clone(),
        extern_regions: BTreeMap::new(),
        tasks: kconfig_tasks,
        shared_regions: BTreeMap::new(),
        // Peripheral interrupts have no host equivalent; software interrupts
        // need a table entry, but no current host task uses them.
        irqs: BTreeMap::new(),
    };
    let kconfig = ron::ser::to_string(&kconfig)?;
    let mut image_id = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&kconfig, &mut image_id);
    let image_id = std::hash::Hasher::finish(&image_id).to_string();

    let kernel_config = cfg.kernel_build_config(&[
        ("HUBRIS_KCONFIG", &kconfig),
        ("HUBRIS_IMAGE_ID", &image_id),
    ]);
    let kernel = build_crate(&kernel_config, &flags.build)?;

    let run_dir = Path::new(TARGET_DIR).join(&toml.name);
    std::fs::create_dir_all(&run_dir)?;
    let config_path = run_dir.join("host-config.ron");
    let host_config = HostConfig {
        tasks: host_tasks,
        stop_at: flags.stop_at,
    };
    std::fs::write(
        &config_path,
        ron::ser::to_string_pretty(&host_config, Default::default())?,
    )?;
    println!("wrote {}", config_path.display());

    if flags.no_run {
        println!(
            "run with: HUBRIS_HOST_CONFIG={} {}",
            config_path.display(),
            kernel.display()
        );
        return Ok(());
    }

    let mut cmd = std::process::Command::new(&kernel);
    cmd.env("HUBRIS_HOST_CONFIG", &config_path);
    if let Some(trace) = &flags.trace {
        cmd.env("HUBRIS_HOST_TRACE", trace);
    }
    println!("running {}", kernel.display());
    let status = cmd.status().context("launching the kernel")?;
    if !status.success() {
        bail!("the kernel exited with {status}");
    }
    Ok(())
}
