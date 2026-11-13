// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Build and/or run tasks compiled for the host's native architecture.
//!
//! A task built this way gets the same features and `HUBRIS_*` environment as
//! it would in `dist` for the given app, so its build script sees the same
//! configuration (task slots, notifications, `[config]` sections). It is then
//! compiled for the host with `userlib`'s host `Arch` implementation, and can
//! be run as a host app using the kernel's host `Arch` implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

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

    let mut cmd = build_config.cmd("rustc");

    cmd.arg("--target-dir").arg(TARGET_DIR);
    if flags.release {
        cmd.arg("--release");
    }

    // As in `dist`, we use attributes to conditionally import based on feature
    // flags; invalid combinations of features often create duplicate
    // attributes, which causes the later one to go unused.  Let's detect this
    // explicitly!
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

/// Name used for the kernel binary.
const KERNEL_EXE: &str = "kernel";

/// Path to the location where all host-built binaries are placed.
fn run_dir(cfg: &PackageConfig) -> PathBuf {
    Path::new(TARGET_DIR).join(&cfg.toml.name)
}

/// Build a single task, placing it in the location specifed by `run_dir()`.
fn build_task(
    cfg: &PackageConfig,
    task: &str,
    flags: &HostBuildFlags,
) -> Result<()> {
    if task == KERNEL_EXE {
        bail!("a host app can't have a task named `{KERNEL_EXE}`");
    }
    let build_config = cfg.task_build_config(task).map_err(|e| anyhow!(e))?;
    let built = build_crate(&build_config, flags)?;
    let exe = run_dir(cfg).join(task);
    std::fs::copy(&built, &exe).with_context(|| {
        format!("copying {} to {}", built.display(), exe.display())
    })?;
    Ok(())
}

/// The task named this in the manifest is the idle task. Isn't actually built,
/// and is handled specially by the kernel.
const IDLE_TASK: &str = "idle";

/// Arguments for `host-run` invocation.
///
/// Includes args that will be passed through to the kernel.
pub struct HostRunFlags {
    pub build: HostBuildFlags,
    /// Arguments for the kernel.
    pub kernel_args: Vec<String>,
    /// Build everything but don't launch the kernel.
    pub no_run: bool,
}

/// Builds every task and the kernel of `app_toml` for the host into the
/// `run_dir()`.
fn build_app(cfg: &PackageConfig, flags: &HostBuildFlags) -> Result<PathBuf> {
    let toml = &cfg.toml;

    let run_dir = run_dir(cfg);
    if run_dir.exists() {
        std::fs::remove_dir_all(&run_dir)
            .with_context(|| format!("removing {}", run_dir.display()))?;
    }
    std::fs::create_dir_all(&run_dir)?;

    let mut kconfig_tasks = Vec::new();
    for (name, task) in &toml.tasks {
        // Skip building the idle task, as it will be faked by the kernel
        // instead.
        if name != IDLE_TASK {
            build_task(cfg, name, flags)?;
        }
        // Memory placement is all kind of made up on the host, since everything
        // is in separate tasks that share nothing anyway.
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
        // Peripheral interrupts have no host equivalent (yet), software
        // interrupts need a table entry, but no current host task uses them
        // (also yet).
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
    let built = build_crate(&kernel_config, flags)?;
    let kernel = run_dir.join(KERNEL_EXE);
    std::fs::copy(&built, &kernel).with_context(|| {
        format!("copying {} to {}", built.display(), kernel.display())
    })?;
    Ok(kernel)
}

/// Builds every task and the kernel of `app_toml` for the host into one
/// directory, and launches the kernel.
pub fn run(app_toml: &Path, flags: HostRunFlags) -> Result<()> {
    let cfg = PackageConfig::new(app_toml, flags.build.verbose, false)?;
    let kernel = build_app(&cfg, &flags.build)?;

    if flags.no_run {
        println!(
            "run with: {} {}",
            kernel.display(),
            flags.kernel_args.join(" ")
        );
        return Ok(());
    }

    println!("running {}", kernel.display());
    let status = std::process::Command::new(&kernel)
        .args(&flags.kernel_args)
        .status()
        .context("launching the kernel")?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => std::process::exit(code),
        None => bail!("the kernel exited with {status}"),
    }
}

/// robo: Arguments for `host-test` invocation.
pub struct HostTestFlags {
    pub build: HostBuildFlags,
    /// robo: Scenarios to run; empty means all of them.
    pub scenarios: Vec<String>,
    /// robo: Virtual time (ms) after which a scenario counts as hung.
    pub stop_at: u64,
    /// robo: Print the output of passing scenarios too.
    pub show_output: bool,
}

/// robo: Task whose `scenarios` config lists the scenarios.
const ORCHESTRATOR_TASK: &str = "orchestrator";

/// robo: Environment variable naming the scenario to run. Must match
/// robo: `host_test_api::harness::SCENARIO_ENV`.
const SCENARIO_ENV: &str = "HUBRIS_HOST_TEST_SCENARIO";

/// robo: Line the supervisor prints when a scenario passes. Must match
/// robo: `host-test-supervisor`.
const PASS_MARKER: &str = "host-test: PASS";

/// robo: Reads the scenario list from the orchestrator's config.
fn scenarios(cfg: &PackageConfig) -> Result<Vec<String>> {
    let task = cfg.toml.tasks.get(ORCHESTRATOR_TASK).ok_or_else(|| {
        anyhow!("a host test image needs a task named `{ORCHESTRATOR_TASK}`")
    })?;
    let list = task
        .config
        .as_ref()
        .and_then(|c| c.get("scenarios"))
        .ok_or_else(|| {
            anyhow!("task `{ORCHESTRATOR_TASK}` has no `scenarios` config")
        })?;
    let ordered_toml::Value::Array(list) = list else {
        bail!("`scenarios` must be an array of strings");
    };
    list.iter()
        .map(|v| match v {
            ordered_toml::Value::String(s) => Ok(s.clone()),
            _ => bail!("`scenarios` must be an array of strings"),
        })
        .collect()
}

/// robo: Builds a host test image and runs the requested scenarios, one
/// robo: kernel run each.
pub fn test(app_toml: &Path, flags: HostTestFlags) -> Result<()> {
    let cfg = PackageConfig::new(app_toml, flags.build.verbose, false)?;
    let all = scenarios(&cfg)?;
    let selected = if flags.scenarios.is_empty() {
        all
    } else {
        if let Some(s) = flags.scenarios.iter().find(|s| !all.contains(s)) {
            bail!("unknown scenario `{s}`; this image has {all:?}");
        }
        flags.scenarios.clone()
    };

    let kernel = build_app(&cfg, &flags.build)?;

    let mut failed = Vec::new();
    for name in &selected {
        let out = std::process::Command::new(&kernel)
            .arg("--stop-at")
            .arg(flags.stop_at.to_string())
            .env(SCENARIO_ENV, name)
            .output()
            .context("launching the kernel")?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        // robo: A run that reaches `--stop-at` also exits 0, so look for
        // robo: the supervisor's pass line too.
        let passed = out.status.success()
            && stderr.lines().any(|l| l.starts_with(PASS_MARKER));
        println!(
            "scenario {name} ... {}",
            if passed { "ok" } else { "FAILED" }
        );
        if !passed || flags.show_output {
            print!("{}", String::from_utf8_lossy(&out.stdout));
            print!("{stderr}");
        }
        if !passed {
            failed.push(name.clone());
        }
    }

    println!(
        "{} passed, {} failed",
        selected.len() - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        bail!("failed scenarios: {}", failed.join(", "));
    }
    Ok(())
}
