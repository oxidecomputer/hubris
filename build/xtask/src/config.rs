// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::hash::Hasher;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
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
    chip: String,
    #[serde(default)]
    signing: IndexMap<String, Signing>,
    secure_separation: Option<bool>,
    stacksize: Option<u32>,
    bootloader: Option<Bootloader>,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    #[serde(default)]
    config: Option<ordered_toml::Value>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub target: String,
    pub board: String,
    pub chip: String,
    pub signing: IndexMap<String, Signing>,
    pub secure_separation: Option<bool>,
    pub stacksize: Option<u32>,
    pub bootloader: Option<Bootloader>,
    pub kernel: Kernel,
    pub outputs: IndexMap<String, Output>,
    pub tasks: IndexMap<String, Task>,
    pub peripherals: IndexMap<String, Peripheral>,
    pub extratext: IndexMap<String, Peripheral>,
    pub config: Option<ordered_toml::Value>,
    pub buildhash: u64,
    pub app_toml_path: PathBuf,
}

impl Config {
    pub fn from_file(cfg: &Path) -> Result<Self> {
        let cfg_contents = std::fs::read(&cfg)?;
        let toml: RawConfig = toml::from_slice(&cfg_contents)?;
        if toml.tasks.contains_key("kernel") {
            bail!("'kernel' is reserved and cannot be used as a task name");
        }

        let mut hasher = DefaultHasher::new();
        hasher.write(&cfg_contents);

        // The app.toml must include a `chip` key, which defines the peripheral
        // register map in a separate file.  We load it then accumulate that
        // file in the buildhash.
        let peripherals = {
            let chip_file =
                cfg.parent().unwrap().join(&toml.chip).join("chip.toml");
            let chip_contents = std::fs::read(chip_file)?;
            hasher.write(&chip_contents);
            toml::from_slice(&chip_contents)?
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

    fn common_build_config<'a>(
        &self,
        verbose: bool,
        crate_name: &str,
        features: &[String],
        sysroot: Option<&'a Path>,
    ) -> BuildConfig<'a> {
        let mut args = vec![
            "--no-default-features".to_string(),
            "--target".to_string(),
            self.target.to_string(),
        ];
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
        env.insert("HUBRIS_TASKS".to_string(), task_names);
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
            env.insert("HUBRIS_APP_CONFIG".to_string(), app_config);
        }

        let out_path = Path::new("")
            .join(&self.target)
            .join("release")
            .join(&crate_name);

        BuildConfig {
            args,
            env,
            crate_name: crate_name.to_string(),
            sysroot,
            out_path,
        }
    }

    pub fn kernel_build_config<'a>(
        &self,
        verbose: bool,
        extra_env: &[(&str, &str)],
        sysroot: Option<&'a Path>,
    ) -> BuildConfig<'a> {
        let mut out = self.common_build_config(
            verbose,
            &self.kernel.name,
            &self.kernel.features,
            sysroot,
        );
        for (var, value) in extra_env {
            out.env.insert(var.to_string(), value.to_string());
        }
        out
    }

    pub fn bootloader_build_config<'a>(
        &self,
        verbose: bool,
        sysroot: Option<&'a Path>,
    ) -> Option<BuildConfig<'a>> {
        self.bootloader.as_ref().map(|bootloader| {
            self.common_build_config(
                verbose,
                &bootloader.name,
                &bootloader.features,
                sysroot,
            )
        })
    }

    pub fn task_build_config<'a>(
        &self,
        task_name: &str,
        verbose: bool,
        sysroot: Option<&'a Path>,
    ) -> Result<BuildConfig<'a>, String> {
        let task_toml = self
            .tasks
            .get(task_name)
            .ok_or_else(|| self.task_name_suggestion(task_name))?;
        let mut out = self.common_build_config(
            verbose,
            &task_toml.name,
            &task_toml.features,
            sysroot,
        );

        //
        // We allow for task- and app-specific configuration to be passed
        // via environment variables to build.rs scripts that may choose to
        // incorporate configuration into compilation.
        //
        if let Some(config) = &task_toml.config {
            let task_config = toml::to_string(&config).unwrap();
            out.env
                .insert("HUBRIS_TASK_CONFIG".to_string(), task_config);
        }

        // Expose the current task's name to allow for better error messages if
        // a required configuration section is missing
        out.env
            .insert("HUBRIS_TASK_NAME".to_string(), task_name.to_string());

        Ok(out)
    }

    /// Returns a map of memory name -> range
    ///
    /// This is useful when allocating memory for tasks
    pub fn memories(&self) -> Result<IndexMap<String, Range<u32>>> {
        self.outputs
            .iter()
            .map(|(name, out)| {
                out.address
                    .checked_add(out.size)
                    .ok_or_else(|| {
                        anyhow!(
                            "output {}: address {:08x} size {:x} would overflow",
                            name,
                            out.address,
                            out.size
                        )
                    })
                    .map(|end| (name.clone(), out.address..end))
            })
            .collect()
    }

    /// Calculates the output region which contains the given address
    pub fn output_region(&self, vaddr: u64) -> Option<&str> {
        let vaddr = u32::try_from(vaddr).ok()?;
        self.outputs
            .iter()
            .find(|(_name, out)| {
                vaddr >= out.address && vaddr < out.address + out.size
            })
            .map(|(name, _out)| name.as_str())
    }

    fn mpu_alignment(&self) -> MpuAlignment {
        // ARMv6-M and ARMv7-M require that memory regions be a power of two.
        // ARMv8-M does not.
        match self.target.as_str() {
            "thumbv8m.main-none-eabihf" => MpuAlignment::Chunk(32),
            "thumbv7em-none-eabihf" | "thumbv6m-none-eabi" => {
                MpuAlignment::PowerOfTwo
            }
            t => panic!("Unknown mpu requirements for target '{}'", t),
        }
    }

    /// Checks whether the given chip's MPU requires power-of-two sized regions
    pub fn mpu_power_of_two_required(&self) -> bool {
        self.mpu_alignment() == MpuAlignment::PowerOfTwo
    }

    /// Suggests an appropriate size for the given task (or "kernel"), given
    /// its true size.  The size depends on MMU implementation, dispatched
    /// based on the `target` in the config file.
    pub fn suggest_memory_region_size(&self, name: &str, size: u64) -> u64 {
        match name {
            "kernel" => {
                // Nearest chunk of 16
                ((size + 15) / 16) * 16
            }
            _ => self.mpu_alignment().suggest_memory_region_size(size),
        }
    }

    /// Returns the desired alignment for a task memory region. This is
    /// dependent on the processor's MMU.
    pub fn task_memory_alignment(&self, size: u32) -> u32 {
        self.mpu_alignment().memory_region_alignment(size)
    }
}

/// Represents an MPU's desired alignment strategy
#[derive(Copy, Clone, Debug, PartialEq)]
enum MpuAlignment {
    /// Regions should be power-of-two sized and aligned
    PowerOfTwo,
    /// Regions should be aligned to chunks with a particular granularity
    Chunk(u64),
}

impl MpuAlignment {
    /// Suggests a minimal memory region size fitting the given number of bytes
    fn suggest_memory_region_size(&self, size: u64) -> u64 {
        match self {
            MpuAlignment::PowerOfTwo => size.next_power_of_two(),
            MpuAlignment::Chunk(c) => ((size + c - 1) / c) * c,
        }
    }
    /// Returns the desired alignment for a region of a particular size
    fn memory_region_alignment(&self, size: u32) -> u32 {
        match self {
            MpuAlignment::PowerOfTwo => {
                assert!(size.is_power_of_two());
                size
            }
            MpuAlignment::Chunk(c) => (*c).try_into().unwrap(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningMethod {
    Crc,
    Rsa,
    Ecc,
}

impl std::fmt::Display for SigningMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Crc => "crc",
            Self::Rsa => "rsa",
            Self::Ecc => "ecc",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Signing {
    pub method: SigningMethod,
    pub priv_key: Option<PathBuf>,
    pub root_cert: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Bootloader {
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
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub features: Vec<String>,
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
    pub name: String,
    #[serde(default)]
    pub max_sizes: IndexMap<String, u32>,
    pub priority: u8,
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
pub struct BuildConfig<'a> {
    pub crate_name: String,

    /// File written by the compiler
    pub out_path: PathBuf,

    args: Vec<String>,
    env: BTreeMap<String, String>,

    /// Optional sysroot to a specific Rust installation.  If this is
    /// specified, then `cargo` is called from the sysroot instead of using
    /// the system façade (which may go through `rustup`).  This saves a few
    /// hundred milliseconds per `cargo` invocation.
    sysroot: Option<&'a Path>,
}

impl BuildConfig<'_> {
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
        let mut cmd = std::process::Command::new(match self.sysroot.as_ref() {
            Some(sysroot) => sysroot.join("bin").join("cargo"),
            None => PathBuf::from("cargo"),
        });

        // nightly features that we use: asm_sym,asm_const,
        // named-profiles,naked_functions,cmse_nonsecure_entry,array_methods
        //
        // nightly features that our dependencies use: backtrace,proc_macro_span

        cmd.arg(
            "-Zallow-features=asm_sym,asm_const,named-profiles,naked_functions,\
cmse_nonsecure_entry,array_methods,backtrace,proc_macro_span",
        );

        cmd.arg(subcommand);
        cmd.arg("-p").arg(&self.crate_name);
        for a in &self.args {
            cmd.arg(a);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}
