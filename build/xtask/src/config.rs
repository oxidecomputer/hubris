// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet, VecDeque};
use std::hash::Hasher;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::auxflash::{build_auxflash, AuxFlash, AuxFlashData};

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
    mmio: Option<MmioConfig>,
    #[serde(default)]
    epoch: u32,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    fwid: bool,
    memory: Option<String>,
    #[serde(default)]
    image_names: Vec<String>,
    #[serde(default)]
    signing: Option<RoTMfgSettings>,
    stacksize: Option<u32>,
    kernel: Kernel,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    config: Option<ordered_toml::Value>,
    auxflash: Option<AuxFlash>,
    caboose: Option<CabooseConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MmioConfig {
    pub peripheral_region: String,
    pub register_map: PathBuf,
}

#[derive(Clone, Debug)]
pub struct MmioData {
    pub base_address: u32,
    pub register_map: PathBuf,
}

/// Data structure of an app's `app.toml` file
#[derive(Clone, Debug)]
pub struct Config {
    /// The name of the app, e.g. oxide-rot-1
    pub name: String,
    pub target: String,
    pub board: String,
    pub chip: String,
    pub epoch: u32,
    pub mmio: Option<MmioData>,
    pub version: u32,
    pub fwid: bool,
    pub image_names: Vec<String>,
    pub signing: Option<RoTMfgSettings>,
    pub stacksize: Option<u32>,
    pub kernel: Kernel,
    pub outputs: IndexMap<String, Vec<Output>>,
    /// Map of tasks, keyed by task name e.g. jefe
    pub tasks: IndexMap<String, Task>,
    pub peripherals: IndexMap<String, Peripheral>,
    pub extratext: IndexMap<String, Peripheral>,
    pub config: Option<ordered_toml::Value>,
    pub buildhash: u64,
    pub app_toml_path: PathBuf,
    /// Fully expanded manifest file, with all patches applied
    pub app_config: String,
    pub auxflash: Option<AuxFlashData>,
    pub caboose: Option<CabooseConfig>,
}

impl Config {
    pub fn archive_name(&self, image_name: &str) -> String {
        assert!(
            self.image_names.iter().any(|s| s == image_name),
            "cannot build archive name for image {image_name:?}: expected one of {:?}",
            self.image_names,
        );
        format!("build-{}-image-{}.zip", self.name, image_name)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CabooseConfig {
    /// List of tasks that are allowed to access the caboose
    #[serde(default)]
    pub tasks: Vec<String>,

    /// Name of the memory region in which the caboose is placed
    ///
    /// (this is almost certainly "flash")
    pub region: String,

    /// Size of the caboose
    ///
    /// The system reserves two words (8 bytes) for the size and marker, so the
    /// user-accessible space is 8 bytes less than this value.
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
        let doc =
            read_and_flatten_toml(cfg, &mut hasher, &mut BTreeSet::new())?;
        let cfg_contents = doc.to_string();

        let toml: RawConfig = toml::from_str(&cfg_contents)?;
        if toml.tasks.contains_key("kernel") {
            bail!("'kernel' is reserved and cannot be used as a task name");
        }

        for (name, size) in &toml.kernel.requires {
            if (size % 4) != 0 {
                bail!("kernel region '{name}' not a multiple of 4: {size}");
            }
        }

        // The app.toml must include a `chip` key, which defines the peripheral
        // register map in a separate file.  We load it then accumulate that
        // file in the buildhash.
        let mut peripherals: IndexMap<String, Peripheral> = {
            let chip_file =
                cfg.parent().unwrap().join(&toml.chip).join("chip.toml");
            let chip_contents = std::fs::read(chip_file)?;
            hasher.write(&chip_contents);
            toml::from_str(std::str::from_utf8(&chip_contents)?)?
        };

        // The manifest may also include a `mmio` key, which defines extra
        // memory-mapped peripherals attached over a memory bus
        let mmio = if let Some(mmio) = &toml.mmio {
            let Some(p) = peripherals.get(&mmio.peripheral_region) else {
                bail!(
                    "could not find peripheral region '{}'",
                    mmio.peripheral_region
                );
            };
            let base_address = p.address;
            use build_fpga_regmap::Node;

            let mmio_file = cfg.parent().unwrap().join(&mmio.register_map);
            let mmio_contents = std::fs::read(&mmio_file)?;
            hasher.write(&mmio_contents);

            let root: Node =
                serde_json::from_str(std::str::from_utf8(&mmio_contents)?)
                    .with_context(|| {
                        format!(
                            "failed to read MMIO register map at {:?}",
                            mmio.register_map
                        )
                    })?;

            let Node::Addrmap { children, .. } = root else {
                bail!("top-level node is not addrmap");
            };
            for (i, p) in children.iter().enumerate() {
                let Node::Addrmap {
                    inst_name,
                    addr_offset,
                    ..
                } = &p
                else {
                    bail!("second-level node must be Addrmap");
                };
                if *addr_offset != i * 256 {
                    bail!("mmio peripherals must be spaced at 256 bytes");
                }
                peripherals.insert(
                    format!("mmio_{inst_name}"),
                    Peripheral {
                        address: *addr_offset as u32 + base_address,
                        size: 256,
                        interrupts: BTreeMap::new(),
                    },
                );
            }
            Some(MmioData {
                base_address,
                register_map: mmio_file.canonicalize()?,
            })
        } else {
            None
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
            toml::from_str(std::str::from_utf8(&chip_contents)?)?
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

        Ok(Config {
            name: toml.name,
            target: toml.target,
            board: toml.board,
            image_names: img_names,
            chip: toml.chip,
            mmio,
            epoch: toml.epoch,
            version: toml.version,
            fwid: toml.fwid,
            signing: toml.signing,
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
            app_config: cfg_contents,
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
        if let Some((_, s)) = scored.first() {
            out.push_str(&format!(" Did you mean '{}'?", s));
        }
        out
    }

    fn common_build_config<'a>(
        &self,
        verbose: bool,
        crate_name: &str,
        no_default_features: bool,
        features: &[String],
        sysroot: Option<&'a Path>,
    ) -> BuildConfig<'a> {
        let mut args = vec!["--target".to_string(), self.target.to_string()];
        if no_default_features {
            args.push("--no-default-features".to_string());
        }
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

        if let Some(mmio) = &self.mmio {
            env.insert(
                "HUBRIS_MMIO_BASE_ADDRESS".to_string(),
                mmio.base_address.to_string(),
            );
            env.insert(
                "HUBRIS_MMIO_REGISTER_MAP".to_string(),
                mmio.register_map.to_str().unwrap().to_owned(),
            );
        }

        if let Some(app_config) = &self.config {
            let app_config = toml::to_string(&app_config).unwrap();
            env.insert("HUBRIS_APP_CONFIG".to_string(), app_config);
        }

        let out_path = Path::new("")
            .join(&self.target)
            .join("release")
            .join(crate_name);

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
            self.kernel.no_default_features,
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
            &task_toml.bin_crate,
            task_toml.no_default_features,
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

        //
        // Expose any external memories that a task is using should the
        // task wish to generate code around them.
        //
        let mut extern_regions = IndexMap::new();

        for name in &task_toml.extern_regions {
            if let Some(r) = self.outputs.get(name) {
                let region = (r[0].address, r[0].size);

                if !r.iter().all(|r| (r.address, r.size) == region) {
                    return Err(format!(
                        "extern region {name} has inconsistent \
                        address/size across images: {r:?}"
                    ));
                }

                extern_regions.insert(name, region);
            }
        }

        out.env.insert(
            "HUBRIS_TASK_EXTERN_REGIONS".to_string(),
            toml::to_string(&extern_regions).unwrap(),
        );

        Ok(out)
    }

    /// Returns a map of memory name -> range for a specific image name
    ///
    /// This is useful when allocating memory for tasks
    pub fn memories(
        &self,
        image_name: &str,
    ) -> Result<IndexMap<String, Range<u32>>> {
        self.outputs
            .iter()
            .map(|(name, out)| {
                let region : Vec<&Output>= out.iter().filter(
                    |o| o.name == *image_name
                ).collect();
                if region.len() > 1 {
                    bail!("Multiple regions defined for image {image_name}");
                }

                if region.is_empty() {
                    bail!("Missing region for {name} in image {image_name}");
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
    /// its true size and a number of available regions.  The size depends on
    /// MMU implementation, dispatched based on the `target` in the config file.
    ///
    /// The returned `Vec<u64>` always has the largest value first.
    pub fn suggest_memory_region_size(
        &self,
        name: &str,
        size: u64,
        regions: usize,
    ) -> VecDeque<u64> {
        match name {
            "kernel" => {
                // Nearest chunk of 16
                [((size + 15) / 16) * 16].into_iter().collect()
            }
            _ => self
                .mpu_alignment()
                .suggest_memory_region_size(size, regions),
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

    pub fn extern_regions_for(
        &self,
        task: &str,
        image_name: &str,
    ) -> Result<IndexMap<String, Range<u32>>> {
        let extern_regions = &self
            .tasks
            .get(task)
            .ok_or_else(|| anyhow!("no such task {task}"))?
            .extern_regions;
        self.get_extern_regions(extern_regions, image_name)
    }

    pub fn kernel_extern_regions(
        &self,
        image_name: &str,
    ) -> Result<IndexMap<String, Range<u32>>> {
        self.get_extern_regions(&self.kernel.extern_regions, image_name)
    }

    fn get_extern_regions(
        &self,
        extern_regions: &Vec<String>,
        image_name: &str,
    ) -> Result<IndexMap<String, Range<u32>>> {
        extern_regions
            .iter()
            .map(|r| {
                let mut regions = self
                    .outputs
                    .get(r)
                    .ok_or_else(|| anyhow!("invalid extern region {r}"))?
                    .iter()
                    .filter(|o| o.name == image_name);
                let out = regions.next().expect("no extern region for name");
                if regions.next().is_some() {
                    bail!(
                        "multiple extern {} regions for name {}",
                        r,
                        image_name
                    );
                }
                Ok((r.to_owned(), out.address..out.address + out.size))
            })
            .collect::<Result<IndexMap<String, Range<u32>>>>()
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
    ///
    /// If multiple regions are available, then we may use them for efficiency.
    /// The resulting `Vec` is guaranteed to have the largest value first.
    fn suggest_memory_region_size(
        &self,
        mut size: u64,
        regions: usize,
    ) -> VecDeque<u64> {
        match self {
            MpuAlignment::PowerOfTwo => {
                const MIN_MPU_REGION_SIZE: u64 = 32;
                let mut out = VecDeque::new();
                for _ in 0..regions {
                    let s =
                        (size.next_power_of_two() / 2).max(MIN_MPU_REGION_SIZE);
                    out.push_back(s);
                    size = size.saturating_sub(s);
                    if size == 0 {
                        break;
                    }
                }
                if size > 0 {
                    if let Some(s) = out.back_mut() {
                        *s *= 2;
                    } else {
                        out.push_back(size.next_power_of_two());
                    }
                }
                // Merge duplicate regions at the end
                while out.len() >= 2 {
                    let n = out.len();
                    if out[n - 1] == out[n - 2] {
                        out.pop_back();
                        *out.back_mut().unwrap() *= 2;
                    } else {
                        break;
                    }
                }
                // Split the initial (largest) region into as many smaller
                // regions as we can fit.  This doesn't change total size, but
                // can make alignment more flexible, since smaller regions have
                // less stringent alignment requirements.
                while out[0] > MIN_MPU_REGION_SIZE {
                    let largest = out[0];
                    let n = out.iter().filter(|c| **c == largest).count();
                    if out.len() + n > regions {
                        break;
                    }
                    // Replace `n` instances of `largest` at the start of `out`
                    // with `n * 2` instances of `largest / 2`
                    for _ in 0..n {
                        out.pop_front();
                    }
                    for _ in 0..n * 2 {
                        out.push_front(largest / 2);
                    }
                }
                out
            }
            MpuAlignment::Chunk(c) => {
                [((size + c - 1) / c) * c].into_iter().collect()
            }
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
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RoTMfgSettings {
    pub certs: lpc55_sign::signed_image::CertConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Kernel {
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub no_default_features: bool,
    #[serde(default)]
    pub extern_regions: Vec<String>,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        nightly_features.extend(["emit_stack_sizes", "used_with_arg"]);
        // nightly features that our dependencies use:
        nightly_features.extend([
            "backtrace",
            "error_generic_member_access",
            "proc_macro_span",
            "proc_macro_span_shrink",
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

////////////////////////////////////////////////////////////////////////////////

fn read_and_flatten_toml(
    cfg: &Path,
    hasher: &mut DefaultHasher,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<toml_edit::Document> {
    use toml_patch::merge_toml_documents;

    // Prevent diamond inheritance
    if !seen.insert(cfg.to_owned()) {
        bail!(
            "{cfg:?} is inherited more than once; \
             diamond dependencies are not allowed"
        );
    }
    let cfg_contents = std::fs::read(cfg)
        .with_context(|| format!("could not read {}", cfg.display()))?;

    // Accumulate the contents into the buildhash here, so that we hash both
    // the inheritance file and the target (recursively, if necessary)
    hasher.write(&cfg_contents);

    let cfg_contents = std::str::from_utf8(&cfg_contents)
        .context("failed to read manifest as UTF-8")?;

    // Additive TOML file inheritance
    let mut doc = cfg_contents
        .parse::<toml_edit::Document>()
        .context("failed to parse TOML file")?;
    let Some(inherited_from) = doc.remove("inherit") else {
        // No further inheritance, so return the current document
        return Ok(doc);
    };

    use toml_edit::{Item, Value};
    let mut original = match inherited_from {
        // Single inheritance
        Item::Value(Value::String(s)) => {
            let file = cfg.parent().unwrap().join(s.value());
            read_and_flatten_toml(&file, hasher, seen)
                .with_context(|| format!("Could not load {file:?}"))?
        }
        // Multiple inheritance, applied sequentially
        Item::Value(Value::Array(a)) => {
            let mut doc: Option<toml_edit::Document> = None;
            for a in a.iter() {
                if let Value::String(s) = a {
                    let file = cfg.parent().unwrap().join(s.value());
                    let next: toml_edit::Document =
                        read_and_flatten_toml(&file, hasher, seen)
                            .with_context(|| {
                                format!("Could not load {file:?}")
                            })?;
                    match doc.as_mut() {
                        Some(doc) => merge_toml_documents(doc, next)?,
                        None => doc = Some(next),
                    }
                } else {
                    bail!("could not inherit from {a}; bad type");
                }
            }
            doc.ok_or_else(|| anyhow!("inherit array cannot be empty"))?
        }
        v => bail!("could not inherit from {v}; bad type"),
    };

    // Finally, apply any changes that are local in this file
    merge_toml_documents(&mut original, doc)?;
    Ok(original)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BoardConfig {
    /// Info about how to interact with this board using probe-rs.
    pub probe_rs: Option<ProbeRsBoardConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProbeRsBoardConfig {
    /// The "chip name" used by probe-rs for flashing.
    pub chip_name: String,
}
