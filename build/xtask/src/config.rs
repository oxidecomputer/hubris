// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::hash::Hasher;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::auxflash::{build_auxflash, AuxFlash, AuxFlashData};
use lpc55_areas::{
    BootSpeed, CFPAPage, CMPAPage, DebugSettings, DefaultIsp, RKTHRevoke,
    ROTKeyStatus, SecureBootCfg,
};

/// A `PatchedConfig` allows a minimal form of inheritance between TOML files
/// Specifically, it allows you to **add features** to specific tasks; nothing
/// else.
///
/// Here's an example:
/// ```toml
/// name = "sidecar-a-lab"
///
/// [patches]
/// inherit = "rev-a.toml"
/// features.sequencer = ["stay-in-a2"]
/// ```
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchedConfig {
    inherit: String,
    patches: ConfigPatches,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatches {
    name: String,
    #[serde(default)]
    features: IndexMap<String, Vec<String>>,
}

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
    epoch: u32,
    #[serde(default)]
    version: u32,
    memory: Option<String>,
    #[serde(default)]
    image_names: Vec<String>,
    #[serde(default)]
    external_images: Vec<String>,
    #[serde(default)]
    signing: Option<RoTMfgSettings>,
    secure_separation: Option<bool>,
    stacksize: Option<u32>,
    kernel: Kernel,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    #[serde(default)]
    config: Option<ordered_toml::Value>,
    #[serde(default)]
    secure_task: Option<String>,
    auxflash: Option<AuxFlash>,
    caboose: Option<CabooseConfig>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub target: String,
    pub board: String,
    pub chip: String,
    pub epoch: u32,
    pub version: u32,
    pub image_names: Vec<String>,
    pub external_images: Vec<String>,
    pub signing: Option<RoTMfgSettings>,
    pub secure_separation: Option<bool>,
    pub stacksize: Option<u32>,
    pub kernel: Kernel,
    pub outputs: IndexMap<String, Vec<Output>>,
    pub tasks: IndexMap<String, Task>,
    pub peripherals: IndexMap<String, Peripheral>,
    pub extratext: IndexMap<String, Peripheral>,
    pub config: Option<ordered_toml::Value>,
    pub buildhash: u64,
    pub app_toml_path: PathBuf,
    pub patches: Option<ConfigPatches>,
    pub secure_task: Option<String>,
    pub auxflash: Option<AuxFlashData>,
    pub dice_mfg: Option<Output>,
    pub caboose: Option<CabooseConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CabooseConfig {
    pub region: String,
    pub size: u32,
}

impl Config {
    pub fn from_file(cfg: &Path) -> Result<Self> {
        Self::from_file_with_hasher(cfg, DefaultHasher::new())
    }

    fn from_file_with_hasher(
        cfg: &Path,
        mut hasher: DefaultHasher,
    ) -> Result<Self> {
        let cfg_contents = std::fs::read(&cfg)
            .with_context(|| format!("could not read {}", cfg.display()))?;

        // Accumulate the contents into the buildhash here, so that we hash both
        // the inheritance file and the target if this is an `PatchedConfig`
        hasher.write(&cfg_contents);

        // Minimal TOML file inheritance, to enable features on a per-task basis
        if let Ok(inherited) = toml::from_slice::<PatchedConfig>(&cfg_contents)
        {
            let file = cfg.parent().unwrap().join(&inherited.inherit);
            let mut original = Config::from_file_with_hasher(&file, hasher)
                .context(format!("Could not load template from {file:?}"))?;
            original.name = inherited.patches.name.to_owned();
            for (task, features) in &inherited.patches.features {
                let t = original
                    .tasks
                    .get_mut(task)
                    .ok_or_else(|| anyhow!("No such task {task}"))?;
                for f in features {
                    if t.features.contains(f) {
                        bail!("Task {task} already contains feature {f}");
                    }
                    t.features.push(f.to_owned());
                }
            }
            original.patches = Some(inherited.patches);
            return Ok(original);
        }

        let toml: RawConfig = toml::from_slice(&cfg_contents)?;
        if toml.tasks.contains_key("kernel") {
            bail!("'kernel' is reserved and cannot be used as a task name");
        }

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

        let outputs: IndexMap<String, Vec<Output>> = {
            let fname = if let Some(n) = toml.memory {
                n
            } else {
                "memory.toml".to_string()
            };
            let chip_file = cfg.parent().unwrap().join(&toml.chip).join(fname);
            let chip_contents =
                std::fs::read(&chip_file).with_context(|| {
                    format!("reading chip file {}", chip_file.display())
                })?;
            hasher.write(&chip_contents);
            toml::from_slice::<IndexMap<String, Vec<Output>>>(&chip_contents)?
        };

        let buildhash = hasher.finish();

        let img_names = if toml.image_names.is_empty() {
            vec!["default".to_string()]
        } else {
            toml.image_names
        };

        // Build the auxiliary flash data so that we can inject it as an
        // environmental variable in the build system.
        let auxflash = match &toml.auxflash {
            Some(a) => Some(build_auxflash(a)?),
            None => None,
        };

        let dice_mfg = match outputs.get("flash") {
            Some(f) => f.iter().find(|&o| o.name == "dice-mfg").cloned(),
            None => None,
        };

        Ok(Config {
            name: toml.name,
            target: toml.target,
            board: toml.board,
            image_names: img_names,
            external_images: toml.external_images,
            chip: toml.chip,
            epoch: toml.epoch,
            version: toml.version,
            signing: toml.signing,
            secure_separation: toml.secure_separation,
            stacksize: toml.stacksize,
            kernel: toml.kernel,
            outputs,
            tasks: toml.tasks,
            peripherals,
            extratext: toml.extratext,
            config: toml.config,
            auxflash,
            buildhash,
            app_toml_path: cfg.to_owned(),
            patches: None,
            secure_task: toml.secure_task,
            dice_mfg,
            caboose: toml.caboose,
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
        env.insert(
            "HUBRIS_BUILD_VERSION".to_string(),
            format!("{}", self.version),
        );
        env.insert("HUBRIS_BUILD_EPOCH".to_string(), format!("{}", self.epoch));
        env.insert("HUBRIS_BOARD".to_string(), self.board.to_string());
        env.insert(
            "HUBRIS_APP_TOML".to_string(),
            app_toml_path.to_str().unwrap().to_string(),
        );
        if let Some(aux) = &self.auxflash {
            env.insert(
                "HUBRIS_AUXFLASH_CHECKSUM".to_string(),
                format!("{:?}", aux.chck),
            );
            for (name, checksum) in aux.checksums.iter() {
                env.insert(
                    format!("HUBRIS_AUXFLASH_CHECKSUM_{}", name),
                    format!("{:?}", checksum),
                );
            }
        }

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

        if let Some(output) = &self.dice_mfg {
            env.insert(
                "HUBRIS_DICE_MFG".to_string(),
                toml::to_string(&output).unwrap(),
            );
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
        let task_config = toml::to_string(&task_toml).unwrap();
        out.env
            .insert("HUBRIS_TASK_CONFIG".to_string(), task_config);

        let all_task_config = toml::to_string(&self.tasks).unwrap();
        out.env
            .insert("HUBRIS_ALL_TASK_CONFIGS".to_string(), all_task_config);

        // Expose the current task's name to allow for better error messages if
        // a required configuration section is missing
        out.env
            .insert("HUBRIS_TASK_NAME".to_string(), task_name.to_string());

        Ok(out)
    }

    /// Returns a map of memory name -> range for a specific image name
    ///
    /// This is useful when allocating memory for tasks
    pub fn memories(
        &self,
        image_name: &String,
    ) -> Result<IndexMap<String, Range<u32>>> {
        self.outputs
            .iter()
            .map(|(name, out)| {
                let region : Vec<&Output>= out.iter().filter(|o| o.name == *image_name).collect();
                if region.len() > 1 {
                    bail!("Multiple regions defined for image {}", image_name);
                }

                let r = region[0];

                r.address
                    .checked_add(r.size)
                    .ok_or_else(|| {
                        anyhow!(
                            "output {}: address {:08x} size {:x} would overflow",
                            name,
                            r.address,
                            r.size
                        )
                    })
                    .map(|end| (name.clone(), r.address..end))
            })
            .collect()
    }

    pub fn all_regions(
        &self,
        region: String,
    ) -> Result<IndexMap<String, Range<u32>>> {
        let outputs: &Vec<Output> = self
            .outputs
            .get(&region)
            .ok_or_else(|| anyhow!("couldn't find region {}", region))?;
        let mut memories: IndexMap<String, Range<u32>> = IndexMap::new();

        for o in outputs {
            memories.insert(o.name.clone(), o.address..o.address + o.size);
        }

        Ok(memories)
    }

    /// Calculates the output region which contains the given address
    pub fn output_region(&self, vaddr: u64) -> Option<&str> {
        let vaddr = u32::try_from(vaddr).ok()?;
        self.outputs
            .iter()
            .find(|(_name, out)| {
                out.iter()
                    .any(|o| vaddr >= o.address && vaddr < o.address + o.size)
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

    pub fn check_image_name(&self, name: &String) -> bool {
        self.image_names.contains(name)
    }

    pub fn need_tz_linker(&self, name: &str) -> bool {
        self.tasks[name].uses_secure_entry
            || self.secure_task.as_ref().map_or(false, |n| n == name)
    }
}

/// Represents an MPU's desired alignment strategy
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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
pub struct RoTMfgSettings {
    pub certs: Vec<lpc55_sign::signed_image::CertChain>,
    #[serde(default)]
    pub enable_secure_boot: bool,
    #[serde(default)]
    pub enable_dice: bool,
    #[serde(default)]
    pub dice_inc_nxp_cfg: bool,
    #[serde(default)]
    pub dice_cust_cfg: bool,
    #[serde(default)]
    pub dice_inc_sec_epoch: bool,
    #[serde(default)]
    pub cmpa_settings: DebugSettings,
    #[serde(default)]
    pub cfpa_settings: DebugSettings,
    #[serde(default = "ROTKeyStatus::enabled")]
    pub rotk0: ROTKeyStatus,
    #[serde(default = "ROTKeyStatus::invalid")]
    pub rotk1: ROTKeyStatus,
    #[serde(default = "ROTKeyStatus::invalid")]
    pub rotk2: ROTKeyStatus,
    #[serde(default = "ROTKeyStatus::invalid")]
    pub rotk3: ROTKeyStatus,
    #[serde(default = "DefaultIsp::auto")]
    pub default_isp: DefaultIsp,
}

impl RoTMfgSettings {
    pub fn generate_cmpa(&self, rkth: &[u8; 32]) -> Result<CMPAPage> {
        let mut cmpa = CMPAPage::new();
        let mut sec_boot = SecureBootCfg::new();

        if self.enable_dice && !self.enable_secure_boot {
            bail!("Must set secure boot to use DICE");
        }

        sec_boot.set_sec_boot(self.enable_secure_boot);
        sec_boot.set_dice(self.enable_dice);
        sec_boot.set_dice_inc_nxp_cfg(self.dice_inc_nxp_cfg);
        sec_boot.set_dice_inc_cust_cfg(self.dice_cust_cfg);
        sec_boot.set_dice_inc_sec_epoch(self.dice_inc_sec_epoch);

        cmpa.set_secure_boot_cfg(sec_boot)?;

        cmpa.set_rotkh(rkth);
        cmpa.set_boot_cfg(self.default_isp, BootSpeed::Fro96mhz)?;

        cmpa.set_debug_fields(self.cmpa_settings)?;

        Ok(cmpa)
    }

    pub fn generate_cfpa(&self) -> Result<CFPAPage> {
        let mut cfpa: CFPAPage = Default::default();

        // We always need to bump the version
        cfpa.update_version();

        let mut rkth = RKTHRevoke::new();

        rkth.rotk0 = self.rotk0.into();
        rkth.rotk1 = self.rotk1.into();
        rkth.rotk2 = self.rotk2.into();
        rkth.rotk3 = self.rotk3.into();

        cfpa.update_rkth_revoke(rkth)?;

        cfpa.set_debug_fields(self.cfpa_settings)?;

        Ok(cfpa)
    }
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

fn default_name() -> String {
    "default".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Output {
    #[serde(default = "default_name")]
    pub name: String,
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
pub struct Peripheral {
    pub address: u32,
    pub size: u32,
    #[serde(default)]
    pub interrupts: BTreeMap<String, u32>,
}

pub use toml_task::Task;

/// Stores arguments and environment variables to run on a particular task.
pub struct BuildConfig<'a> {
    pub crate_name: String,

    /// File written by the compiler
    pub out_path: PathBuf,

    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,

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

        let mut nightly_features = vec![];
        // nightly features that we use:
        nightly_features.extend([
            "array_methods",
            "asm_const",
            "cmse_nonsecure_entry",
            "naked_functions",
            "named-profiles",
        ]);
        // nightly features that our dependencies use:
        nightly_features.extend([
            "backtrace",
            "error_generic_member_access",
            "proc_macro_span",
            "provide_any",
        ]);

        cmd.arg(format!("-Zallow-features={}", nightly_features.join(",")));

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
