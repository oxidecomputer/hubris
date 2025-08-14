// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;

/// Reads the given environment variable and marks that it's used
///
/// This ensures a rebuild if the variable changes
pub fn env_var(key: &str) -> Result<String> {
    println!("cargo::rerun-if-env-changed={key}");
    std::env::var(key).with_context(|| format!("reading env var ${key}"))
}

/// Reads the `OUT_DIR` environment variable
///
/// This function goes through `std::env::var` directly, rather than our own
/// `env_var`, because Cargo should know when it changes.
pub fn out_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("OUT_DIR").expect("Could not get OUT_DIR"),
    )
}

/// Reads the `TARGET` environment variable
///
/// This function goes through `std::env::var` directly, rather than our own
/// `env_var`, because Cargo should know when `OUT_DIR` changes.
pub fn target() -> String {
    std::env::var("TARGET").unwrap()
}

/// Reads the `TARGET_OS` environment variable
///
/// This function goes through `std::env::var` directly, rather than our own
/// `env_var`, because Cargo should know when it changes.
pub fn target_os() -> String {
    std::env::var("CARGO_CFG_TARGET_OS").unwrap()
}

/// Reads the `HUBRIS_TASK_NAME` env var.
pub fn task_name() -> String {
    crate::env_var("HUBRIS_TASK_NAME").expect("missing HUBRIS_TASK_NAME")
}

/// Checks to see whether the given feature is enabled
pub fn has_feature(s: &str) -> bool {
    std::env::var(format!(
        "CARGO_FEATURE_{}",
        s.to_uppercase().replace('-', "_")
    ))
    .is_ok()
}

/// Exposes the CPU's M-profile architecture version. This isn't available in
/// rustc's standard environment.
///
/// This will set one of `cfg(armv6m)`, `cfg(armv7m)`, or `cfg(armv8m)`
/// depending on the value of the `TARGET` environment variable.
pub fn expose_m_profile() -> Result<()> {
    let target = crate::target();

    println!("cargo::rustc-check-cfg=cfg(armv6m)");
    println!("cargo::rustc-check-cfg=cfg(armv7m)");
    println!("cargo::rustc-check-cfg=cfg(armv8m)");

    if target.starts_with("thumbv6m") {
        println!("cargo::rustc-cfg=armv6m");
    } else if target.starts_with("thumbv7m") || target.starts_with("thumbv7em")
    {
        println!("cargo::rustc-cfg=armv7m");
    } else if target.starts_with("thumbv8m") {
        println!("cargo::rustc-cfg=armv8m");
    } else {
        bail!("Don't know the target {target}");
    }
    Ok(())
}

/// Returns the `HUBRIS_BOARD` envvar, if set.
pub fn target_board() -> Option<String> {
    crate::env_var("HUBRIS_BOARD").ok()
}

/// Exposes the board type from the `HUBRIS_BOARD` envvar into
/// `cfg(target_board="...")`.
pub fn expose_target_board() {
    let mut boards = vec![];
    let mut out_dir =
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    loop {
        let done = out_dir.file_name() == Some(OsStr::new("target"));
        if !out_dir.pop() {
            panic!("can't find target/ in OUT_DIR");
        }
        if done {
            break;
        }
    }
    out_dir.push("boards");
    println!("cargo::rerun-if-changed={}", out_dir.display());
    if let Ok(dir) = std::fs::read_dir(&out_dir) {
        for dirent in dir {
            let Ok(dirent) = dirent else {
                eprintln!("warning: bogus dirent?");
                continue;
            };
            let path = dirent.path();

            if path.extension() != Some(OsStr::new("toml")) {
                // Ignore other files, such as READMEs, in the directory.
                continue;
            }

            if let Some(stem) = path.file_stem() {
                let stem = stem.to_string_lossy();
                boards.push(format!("\"{stem}\""));
            }
        }
    } else {
        eprintln!(
            "warning: boards directory not found, \
                  target_board can't validate"
        );
    }
    boards.sort();
    boards.dedup();

    let values = boards.join(",");

    println!("cargo::rustc-check-cfg=cfg(target_board, values({values}))");
    if let Some(board) = target_board() {
        println!("cargo::rustc-cfg=target_board=\"{board}\"");
    }
}

///
/// Pulls the app-wide configuration for purposes of a build task.  This
/// will fail if the app-wide configuration doesn't exist or can't parse.
/// Note that -- thanks to the magic of Serde -- `T` need not (and indeed,
/// should not) contain the entire app-wide configuration, but rather only
/// those parts that a particular build task cares about.  (It should go
/// without saying that `deny_unknown_fields` should *not* be set on this
/// type -- but it may well be set within the task-specific types that
/// this type contains.)  If the configuration field is optional, `T` should
/// reflect that by having its member (or members) be an `Option` type.
///
pub fn config<T: DeserializeOwned>() -> Result<T> {
    toml_from_env("HUBRIS_APP_CONFIG")?.ok_or_else(|| {
        anyhow!("app.toml missing global config section [config]")
    })
}

/// Pulls the task configuration. See `config` for more details.
pub fn task_config<T: DeserializeOwned>() -> Result<T> {
    task_maybe_config()?.ok_or_else(|| {
        anyhow!(
            "app.toml missing task config section [tasks.{}.config]",
            task_name()
        )
    })
}

/// Pulls the task configuration, or `None` if the configuration is not
/// provided.
pub fn task_maybe_config<T: DeserializeOwned>() -> Result<Option<T>> {
    let t = toml_from_env::<toml_task::Task<T>>("HUBRIS_TASK_CONFIG")?;
    Ok(t.and_then(|t| t.config))
}

/// Pulls the full task configuration block for the current task
///
/// (compare with `task_maybe_config`, which returns just the `config`
/// subsection)
pub fn task_full_config<T: DeserializeOwned>() -> Result<toml_task::Task<T>> {
    let t = toml_from_env::<toml_task::Task<T>>("HUBRIS_TASK_CONFIG")?
        .ok_or_else(|| anyhow!("HUBRIS_TASK_CONFIG is not defined"))?;
    Ok(t)
}

/// Pulls the full task configuration block, with the `config` subsection
/// encoded as a TOML `Value`
pub fn task_full_config_toml() -> Result<toml_task::Task<ordered_toml::Value>> {
    task_full_config()
}

/// Pulls the external regions that the task is using
pub fn task_extern_regions<T: DeserializeOwned>() -> Result<IndexMap<String, T>>
{
    let t = toml_from_env::<IndexMap<String, T>>("HUBRIS_TASK_EXTERN_REGIONS")?
        .ok_or_else(|| anyhow!("HUBRIS_TASK_EXTERN_REGIONS is not defined"))?;

    Ok(t)
}

/// Pulls the full task configuration block of a different task
pub fn other_task_full_config<T: DeserializeOwned>(
    name: &str,
) -> Result<toml_task::Task<T>> {
    let mut t = toml_from_env::<IndexMap<String, toml_task::Task<_>>>(
        "HUBRIS_ALL_TASK_CONFIGS",
    )?
    .ok_or_else(|| anyhow!("HUBRIS_ALL_TASK_CONFIGS is not defined"))?;
    let out = t
        .remove(name)
        .ok_or_else(|| anyhow!("Could not find {name} in tasks"))?;
    Ok(out)
}

pub fn other_task_full_config_toml(
    name: &str,
) -> Result<toml_task::Task<ordered_toml::Value>> {
    other_task_full_config(name)
}

/// Returns a map of task names to their IDs.
pub fn task_ids() -> TaskIds {
    let tasks = crate::env_var("HUBRIS_TASKS").expect("missing HUBRIS_TASKS");
    TaskIds(
        tasks
            .split(',')
            .enumerate()
            .map(|(i, name)| (name.to_string(), i))
            .collect(),
    )
}

/// Map of task names to their IDs.
pub struct TaskIds(BTreeMap<String, usize>);

impl TaskIds {
    /// Get the ID of a task by name.
    pub fn get(&self, task_name: &str) -> Option<usize> {
        self.0.get(task_name).copied()
    }

    /// Convert a list of task names into a list of task IDs, ordered the same.
    pub fn names_to_ids<S>(&self, names: &[S]) -> Result<Vec<usize>>
    where
        S: AsRef<str>,
    {
        names
            .iter()
            .map(|name| {
                let name = name.as_ref();
                self.get(name)
                    .ok_or_else(|| anyhow!("unknown task `{}`", name))
            })
            .collect()
    }

    /// Helper function to convert a map of operation names to allowed callers
    /// (by name) to a map of operation names to allowed callers (by task ID).
    pub fn remap_allowed_caller_names_to_ids(
        &self,
        allowed_callers: &BTreeMap<String, Vec<String>>,
    ) -> Result<BTreeMap<String, Vec<usize>>> {
        allowed_callers
            .iter()
            .map(|(name, tasks)| {
                let task_ids = self.names_to_ids(tasks)?;
                Ok((name.clone(), task_ids))
            })
            .collect()
    }
}

/// Parse the contents of an environment variable as toml.
///
/// Returns:
///
/// - `Ok(Some(x))` if the environment variable is defined and the contents
///   deserialized correctly.
/// - `Ok(None)` if the environment variable is not defined.
/// - `Err(e)` if deserialization failed or the environment variable did not
///   contain UTF-8.
fn toml_from_env<T: DeserializeOwned>(var: &str) -> Result<Option<T>> {
    let config = match crate::env_var(var) {
        Err(e) => {
            use std::env::VarError;

            return if e.downcast_ref::<std::env::VarError>()
                == Some(&VarError::NotPresent)
            {
                Ok(None)
            } else {
                Err(e).context("reading TOML from build environment")
            };
        }
        Ok(c) => c,
    };

    let rval =
        toml::from_str(&config).context("deserializing configuration")?;
    Ok(Some(rval))
}

pub fn build_notifications() -> Result<()> {
    let out_dir = out_dir();
    let dest_path = out_dir.join("notifications.rs");
    let mut out = std::fs::File::create(dest_path)?;

    let full_task_config = task_full_config_toml()?;

    if full_task_config.notifications.len() >= 32 {
        bail!(
            "Too many notifications; \
             overlapping with `INTERNAL_TIMER_NOTIFICATION`"
        );
    }
    if full_task_config.bin_crate == "task-jefe"
        && full_task_config.notifications.first().cloned()
            != Some("fault".to_string())
    {
        bail!("`jefe` must have \"fault\" as its first notification");
    }

    writeln!(&mut out, "#[allow(dead_code)]")?;
    writeln!(&mut out, "pub mod notifications {{")?;

    write_task_notifications(&mut out, &full_task_config.notifications)?;

    for task in env_var("HUBRIS_TASKS")
        .expect("missing HUBRIS_TASKS")
        .split(',')
    {
        let full_task_config = other_task_full_config_toml(task)?;
        writeln!(&mut out, "pub mod {task} {{")?;
        write_task_notifications(&mut out, &full_task_config.notifications)?;
        writeln!(&mut out, "}}")?;
    }

    writeln!(&mut out, "}}")?;

    Ok(())
}

fn write_task_notifications<W: Write>(out: &mut W, t: &[String]) -> Result<()> {
    if t.len() > 32 {
        bail!("Too many notifications; cannot fit in a `u32` mask");
    }
    for (i, n) in t.iter().enumerate() {
        let n = n.to_uppercase().replace('-', "_");
        writeln!(out, "pub const {n}_BIT: u8 = {i};")?;
        writeln!(out, "pub const {n}_MASK: u32 = 1 << {n}_BIT;")?;
    }
    Ok(())
}
