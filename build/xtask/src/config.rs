// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::hash::Hasher;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use indexmap::IndexMap;
use serde::Deserialize;

/// A `RawConfig` represents an `app.toml` file that has been deserialized,
/// but may not be ready for use.  In particular, we use the `chip` field
/// to load a second file containing peripheral register addresses.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawConfig {
    name: String,
    target: String,
    board: String,
    #[serde(default)]
    chip: Option<String>,
    #[serde(default)]
    signing: IndexMap<String, Signing>,
    secure_separation: Option<bool>,
    stacksize: Option<u32>,
    bootloader: Option<Bootloader>,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
    #[serde(default)]
    config: Option<ordered_toml::Value>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub target: String,
    pub board: String,
    pub chip: Option<String>,
    pub signing: IndexMap<String, Signing>,
    pub secure_separation: Option<bool>,
    pub stacksize: Option<u32>,
    pub bootloader: Option<Bootloader>,
    pub kernel: Kernel,
    pub outputs: IndexMap<String, Output>,
    pub tasks: IndexMap<String, Task>,
    pub peripherals: IndexMap<String, Peripheral>,
    pub extratext: IndexMap<String, Peripheral>,
    pub supervisor: Option<Supervisor>,
    pub config: Option<ordered_toml::Value>,
    pub buildhash: u64,
    pub app_toml_path: PathBuf,
}

impl Config {
    pub fn from_file(cfg: &Path) -> Result<Self> {
        let cfg_contents = std::fs::read(&cfg)?;
        let toml: RawConfig = toml::from_slice(&cfg_contents)?;

        let mut hasher = DefaultHasher::new();
        hasher.write(&cfg_contents);

        // If the app.toml specifies a `chip` key, then load the peripheral
        // register map from a separate file and accumulate that file in the
        // buildhash.
        let peripherals = if let Some(chip) = &toml.chip {
            if !toml.peripherals.is_empty() {
                bail!("Cannot specify both chip and peripherals");
            }
            let chip_file = cfg.parent().unwrap().join(chip).join("chip.toml");
            let chip_contents = std::fs::read(chip_file)?;
            hasher.write(&chip_contents);
            toml::from_slice(&chip_contents)?
        } else {
            toml.peripherals
        };

        let buildhash = hasher.finish();

        Ok(Config {
            name: toml.name,
            target: toml.target,
            board: toml.board,
            chip: toml.chip,
            signing: toml.signing,
            secure_separation: toml.secure_separation,
            stacksize: toml.stacksize,
            bootloader: toml.bootloader,
            kernel: toml.kernel,
            outputs: toml.outputs,
            tasks: toml.tasks,
            peripherals,
            extratext: toml.extratext,
            supervisor: toml.supervisor,
            config: toml.config,
            buildhash,
            app_toml_path: cfg.to_owned(),
        })
    }

    pub fn task_name_suggestion(&self, name: &str) -> String {
        // Suggest only for very small differences
        // High number can result in inaccurate suggestions for short queries e.g. `rls`
        const MAX_DISTANCE: usize = 3;

        let mut scored: Vec<_> = self
            .tasks
            .keys()
            .filter_map(|s| {
                let distance = strsim::damerau_levenshtein(name, s);
                if distance <= MAX_DISTANCE {
                    Some((distance, s))
                } else {
                    None
                }
            })
            .collect();
        scored.sort();
        let mut out = format!("'{}' is not a valid task name.", name);
        if let Some((_, s)) = scored.get(0) {
            out.push_str(&format!(" Did you mean '{}'?", s));
        }
        out
    }

    fn common_build_config(
        &self,
        verbose: bool,
        crate_name: &str,
        relative_path: &Path,
        features: &[String],
    ) -> BuildConfig {
        let mut args = Vec::new();
        args.push("--no-default-features".to_string());
        args.push("--target".to_string());
        args.push(self.target.to_string());
        if verbose {
            args.push("-v".to_string());
        }

        if !features.is_empty() {
            args.push("--features".to_string());
            args.push(features.join(","));
        }

        let mut env = BTreeMap::new();

        // We include the path to the configuration TOML file so that proc macros
        // that use it can easily force a rebuild (using include_bytes!)
        //
        // This doesn't matter now, because we rebuild _everything_ on app.toml
        // changes, but once #240 is closed, this will be important.
        let app_toml_path = self
            .app_toml_path
            .canonicalize()
            .expect("Could not canonicalize path to app TOML file");

        let task_names =
            self.tasks.keys().cloned().collect::<Vec<_>>().join(",");
        env.insert("HUBRIS_TASKS".to_string(), task_names.to_string());
        env.insert("HUBRIS_BOARD".to_string(), self.board.to_string());
        env.insert(
            "HUBRIS_APP_TOML".to_string(),
            app_toml_path.to_str().unwrap().to_string(),
        );

        // secure_separation indicates that we have TrustZone enabled.
        // When TrustZone is enabled, the bootloader is secure and hubris is
        // not secure.
        // When TrustZone is not enabled, both the bootloader and Hubris are
        // secure.
        if let Some(s) = self.secure_separation {
            if s {
                env.insert("HUBRIS_SECURE".to_string(), "0".to_string());
            } else {
                env.insert("HUBRIS_SECURE".to_string(), "1".to_string());
            }
        } else {
            env.insert("HUBRIS_SECURE".to_string(), "1".to_string());
        }

        if let Some(app_config) = &self.config {
            let app_config = toml::to_string(&app_config).unwrap();
            env.insert("HUBRIS_APP_CONFIG".to_string(), app_config.to_string());
        }

        let mut crate_path = self.app_toml_path.clone();
        crate_path.pop();
        crate_path.push(relative_path);

        let mut out_path = Path::new("").to_path_buf();
        out_path.push(&self.target);
        out_path.push("release");
        out_path.push(crate_name);

        BuildConfig {
            args,
            env,
            crate_path,
            out_path,
        }
    }

    pub fn kernel_build_config(
        &self,
        verbose: bool,
        extra_env: &[(&str, &str)],
    ) -> BuildConfig {
        let mut out = self.common_build_config(
            verbose,
            &self.kernel.name,
            &self.kernel.path,
            &self.kernel.features,
        );
        for (var, value) in extra_env {
            out.env.insert(var.to_string(), value.to_string());
        }
        out
    }

    pub fn bootloader_build_config(
        &self,
        verbose: bool,
    ) -> Option<BuildConfig> {
        self.bootloader.as_ref().map(|bootloader| {
            self.common_build_config(
                verbose,
                &bootloader.name,
                &bootloader.path,
                &bootloader.features,
            )
        })
    }

    pub fn task_build_config(
        &self,
        task_name: &str,
        verbose: bool,
    ) -> Result<BuildConfig, String> {
        let task_toml = self
            .tasks
            .get(task_name)
            .ok_or_else(|| self.task_name_suggestion(task_name))?;
        let mut out = self.common_build_config(
            verbose,
            &task_toml.name,
            &task_toml.path,
            &task_toml.features,
        );

        //
        // We allow for task- and app-specific configuration to be passed
        // via environment variables to build.rs scripts that may choose to
        // incorporate configuration into compilation.
        //
        if let Some(config) = &task_toml.config {
            let task_config = toml::to_string(&config).unwrap();
            out.env.insert(
                "HUBRIS_TASK_CONFIG".to_string(),
                task_config.to_string(),
            );
        }

        // Expose the current task's name to allow for better error messages if
        // a required configuration section is missing
        out.env
            .insert("HUBRIS_TASK_NAME".to_string(), task_name.to_string());

        Ok(out)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Signing {
    pub method: String,
    pub priv_key: Option<PathBuf>,
    pub root_cert: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Bootloader {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub sections: IndexMap<String, String>,
    #[serde(default)]
    pub imagea_flash_start: u32,
    pub imagea_flash_size: u32,
    pub imagea_ram_start: u32,
    pub imagea_ram_size: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Kernel {
    pub path: PathBuf,
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Supervisor {
    pub notification: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Output {
    pub address: u32,
    pub size: u32,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub execute: bool,
    #[serde(default)]
    pub dma: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Task {
    pub path: PathBuf,
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub priority: u32,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub start: bool,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub interrupts: IndexMap<String, u32>,
    #[serde(default)]
    pub sections: IndexMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_task_slot")]
    pub task_slots: IndexMap<String, String>,
    #[serde(default)]
    pub config: Option<ordered_toml::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Peripheral {
    pub address: u32,
    pub size: u32,
    #[serde(default)]
    pub interrupts: BTreeMap<String, u32>,
}

/// In the common case, task slots map back to a task of the same name (e.g.
/// `gpio_driver`, `rcc_driver`).  However, certain tasks need generic task
/// slot names, e.g. they'll have a task slot named `spi_driver` which will
/// be mapped to a specific SPI driver task (`spi2_driver`).
///
/// This deserializer lets us handle both cases, while making the common case
/// easiest to write.  In `app.toml`, you can write something like
/// ```toml
/// task-slots = [
///     "gpio_driver",
///     "i2c_driver",
///     "rcc_driver",
///     {spi_driver: "spi2_driver"},
/// ]
/// ```
fn deserialize_task_slot<'de, D>(
    deserializer: D,
) -> Result<IndexMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Clone, Debug, Deserialize)]
    #[serde(untagged)]
    enum ArrayItem {
        Identity(String),
        Remap(IndexMap<String, String>),
    }
    let s: Vec<ArrayItem> = serde::Deserialize::deserialize(deserializer)?;
    let mut out = IndexMap::new();
    for a in s {
        match a {
            ArrayItem::Identity(s) => {
                out.insert(s.clone(), s.clone());
            }
            ArrayItem::Remap(m) => {
                if m.len() != 1 {
                    return Err(serde::de::Error::invalid_length(
                        m.len(),
                        &"a single key-value pair",
                    ));
                }
                let (k, v) = m.iter().next().unwrap();
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(out)
}

/// Stores arguments and environment variables to run on a particular task.
pub struct BuildConfig {
    args: Vec<String>,
    env: BTreeMap<String, String>,
    pub crate_path: PathBuf,
    pub out_path: PathBuf,
}

impl BuildConfig {
    /// Applies the arguments and environment to a given Command
    pub fn cmd(&self, subcommand: &str) -> std::process::Command {
        // NOTE: current_dir's docs suggest that you should use canonicalize
        // for portability. However, that's for when you're doing stuff like:
        //
        // Command::new("../cargo")
        //
        // That is, when you have a relative path to the binary being executed.
        // We are not including a path in the binary name, so everything is
        // peachy. If you change this line below, make sure to canonicalize
        // path.
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg(subcommand);
        for a in &self.args {
            cmd.arg(a);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.current_dir(&self.crate_path);
        cmd
    }
}
