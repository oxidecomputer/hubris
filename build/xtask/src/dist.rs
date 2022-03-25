// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, VecDeque};
use std::convert::TryInto;
use std::fs::{self, File};
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use indexmap::IndexMap;
use path_slash::PathBufExt;
use serde::Serialize;

use crate::{elf, task_slot, Config, LoadSegment, Signing, SizedTask};

use lpc55_sign::{crc_image, signed_image};

/// In practice, applications with active interrupt activity tend to use about
/// 650 bytes of stack. Because kernel stack overflows are annoying, we've
/// padded that a bit.
pub const DEFAULT_KERNEL_STACK: u32 = 1024;

struct Packager {
    /// Sysroot of the relevant toolchain
    sysroot: PathBuf,

    /// Host triple, e.g. `aarch64-apple-darwin`
    host_triple: String,

    /// Enables -v when calling subcommands
    verbose: bool,

    /// Print build graph edges using `cargo tree` before building
    edges: bool,

    /// Full app configuration
    config: Config,

    /// The TOML file responsible for configuration
    cfg_file: PathBuf,

    /// Individual sections of the image and their addresses
    all_output_sections: BTreeMap<u32, LoadSegment>,

    /// Task entry points, populated by [link_task]
    entry_points: BTreeMap<String, u32>,

    /// Represents memory requirements for a particular task
    task_requires: BTreeMap<String, IndexMap<String, u32>>,

    /// Represents actual memory allocated to tasks
    allocations: Allocations,
}

struct BuildConfig {
    with_task_names: bool,
    with_secure_separation: bool,
    with_shared_syms: bool,
}

impl Packager {
    /// Loads the given configuration from a file
    pub fn init(verbose: bool, edges: bool, cfg: &Path) -> Result<Self> {
        let sysroot = Command::new("rustc")
            .arg("--print")
            .arg("sysroot")
            .output()?;
        if !sysroot.status.success() {
            bail!("Could not find execute rustc to get sysroot");
        }
        let sysroot =
            PathBuf::from(std::str::from_utf8(&sysroot.stdout)?.trim());

        let host = Command::new("rustc").arg("-vV").output()?;
        if !host.status.success() {
            bail!("Could not execute rustc to get host");
        }
        let host_triple = std::str::from_utf8(&host.stdout)?
            .split('\n')
            .flat_map(|line| line.strip_prefix("host: "))
            .next()
            .ok_or_else(|| anyhow!("Could not get host from rustc"))?
            .to_string();

        Ok(Self {
            sysroot,
            host_triple,
            verbose,
            edges,
            config: Config::from_file(&cfg)?,
            cfg_file: cfg.to_path_buf(),
            all_output_sections: BTreeMap::default(),
            entry_points: BTreeMap::default(),
            task_requires: BTreeMap::default(),
            allocations: Allocations::default(),
        })
    }

    /// Returns the directory in which the app configuration file is stored
    fn cfg_dir(&self) -> PathBuf {
        let mut out = self.cfg_file.clone();
        out.pop();
        out
    }

    /// Returns the output directory for this build
    fn out_dir(&self) -> PathBuf {
        let mut out_dir = PathBuf::from("target");
        out_dir.push(&self.config.name);
        out_dir.push("dist");
        out_dir
    }

    /// Returns a `PathBuf` to a file in the output directory
    fn out_file(&self, filename: &str) -> PathBuf {
        let mut out = self.out_dir();
        out.push(filename);
        out
    }

    /// Constructs the output directory for this build, if not present
    fn create_out_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.out_dir())
            .context("Could not create output dir")?;
        Ok(())
    }

    /// Cleans the build if the build hash has changed (or isn't present)
    fn check_rebuild(&self) -> Result<()> {
        if self.needs_rebuild() {
            self.clean_build()?;
            std::fs::write(
                Self::buildstamp_file(),
                format!("{:x}", self.config.buildhash),
            )
            .context("Could not write buildstamp file")?;
        }
        Ok(())
    }

    /// Checks whether this is a valid partial build.  If tasks_to_build is
    /// Some(...), every build task must be in our task list.
    ///
    /// Returns true if this is a partial build, false otherwise
    fn check_partial_build(
        &self,
        tasks_to_build: &Option<Vec<String>>,
    ) -> Result<bool> {
        if let Some(included_names) = tasks_to_build {
            let all_tasks = self.config.tasks.keys().collect::<Vec<_>>();
            if let Some(name) =
                included_names.iter().find(|n| !all_tasks.contains(n))
            {
                Err(anyhow!(
                    "Attempted to build task '{}', which is not in the app",
                    name
                ))
            } else {
                Ok(true)
            }
        } else {
            Ok(false)
        }
    }

    /// Returns the path of the buildstamp file, which stores a hash associated
    /// with the `app.toml`
    fn buildstamp_file() -> PathBuf {
        Path::new("target").join("buildstamp")
    }

    /// Checks to see whether we need a clean rebuild, based on the buildstamp
    /// file's presence and hash value.
    fn needs_rebuild(&self) -> bool {
        match std::fs::read(Self::buildstamp_file()) {
            Ok(contents) => {
                if let Ok(contents) = std::str::from_utf8(&contents) {
                    if let Ok(cmp) = u64::from_str_radix(contents, 16) {
                        self.config.buildhash != cmp
                    } else {
                        println!(
                            "buildstamp file contents unknown; re-building."
                        );
                        true
                    }
                } else {
                    println!("buildstamp file contents corrupt; re-building.");
                    true
                }
            }
            Err(_) => {
                println!("no buildstamp file found; re-building.");
                true
            }
        }
    }

    /// Runs `cargo clean` on all targets (tasks, kernel, and bootloader)
    fn clean_build(&self) -> Result<()> {
        println!("app.toml has changed; rebuilding all tasks");

        let mut todo = vec![];
        todo.push(self.config.kernel.name.as_str());
        todo.extend(self.config.tasks.values().map(|t| t.name.as_str()));
        todo.extend(
            self.config
                .bootloader
                .as_ref()
                .map(|b| b.name.as_str())
                .iter(),
        );

        todo.into_iter().map(|n| self.cargo_clean(n)).collect()
    }

    fn get_task_requires(
        &self,
        task_name: &str,
        stacksize: u32,
    ) -> Result<IndexMap<String, u32>> {
        use goblin::Object;
        let task_elf_file = self.out_file(task_name);
        let buffer = std::fs::read(&task_elf_file)
            .context(format!("Could not read task ELF {:?}", task_elf_file))?;
        let elf = match Object::parse(&buffer)? {
            Object::Elf(elf) => elf,
            o => bail!("Invalid Object {:?}", o),
        };

        // Find the output region that a particular vaddr lives in
        let output_region = |vaddr: u64| {
            self.config
                .outputs
                .keys()
                .map(|name| (name, self.get_memory(name).unwrap()))
                .find(|(_, region)| region.contains(&vaddr.try_into().unwrap()))
                .map(|(name, _)| name.as_str())
        };

        let mut memory_sizes: IndexMap<String, u32> = IndexMap::new();
        for phdr in &elf.program_headers {
            if let Some(vregion) = output_region(phdr.p_vaddr) {
                let d: u32 = phdr.p_memsz.try_into().unwrap();
                *memory_sizes.entry(vregion.to_string()).or_default() += d;
            }
            // If the VirtAddr disagrees with the PhysAddr, then this is a
            // section which is relocated into RAM, so we also accumulate
            // its FileSiz in the physical address (which is presumably
            // flash).
            if phdr.p_vaddr != phdr.p_paddr {
                let region = output_region(phdr.p_paddr).unwrap();
                let d: u32 = phdr.p_filesz.try_into().unwrap();
                *memory_sizes.entry(region.to_string()).or_default() += d;
            }
        }
        *memory_sizes.entry(String::from("ram")).or_default() += stacksize;

        // Round up all memory allocation sizes to be powers of two
        // TODO: don't do this for chips with other rounding strategies
        for v in memory_sizes.values_mut() {
            *v = v.checked_next_power_of_two().unwrap();
        }
        Ok(memory_sizes)
    }

    /// Writes [allocations] based on tasks
    fn allocate_all(&mut self) -> Result<()> {
        assert!(self.allocations.tasks.is_empty());
        assert!(self.task_requires.is_empty());

        // Build an allocations table which gives the entirety of flash
        // and RAM to every task.
        let infinite_space = self
            .config
            .outputs
            .keys()
            .map(|name| (name.clone(), self.get_memory(name).unwrap()))
            .collect::<BTreeMap<_, _>>();

        // Do a dummy link of each task, just to get the size of the
        // resulting file.
        for (name, task) in &self.config.tasks {
            fs::copy("build/task-link.x", self.out_file("link.x"))
                .context("Could not copy task-link.x")?;
            generate_task_linker_script(
                self.out_file("memory.x"),
                &infinite_space,
                Some(&task.sections),
                task.stacksize.or(self.config.stacksize).ok_or_else(|| {
                    anyhow!(
                        "{}: no stack size specified and there is no default",
                        name
                    )
                })?,
            )
            .context(format!(
                "failed to generate linker script for {}",
                name
            ))?;

            // Do the actual work of linking here
            self.link(&name)?;

            let r = self.get_task_requires(
                &name,
                task.stacksize.or(self.config.stacksize).ok_or_else(|| {
                    anyhow!(
                        "{}: no stack size specified and there is no default",
                        name
                    )
                })?,
            )?;
            self.task_requires.insert(name.clone(), r);
        }

        let tasks: IndexMap<String, SizedTask> = self
            .config
            .tasks
            .iter()
            .map(|(name, task)| {
                (
                    name.clone(),
                    SizedTask {
                        task: task.clone(),
                        requires: self.task_requires[name].clone(),
                    },
                )
            })
            .collect();
        let mut free = self
            .config
            .outputs
            .keys()
            .map(|name| (name.clone(), self.get_memory(name).unwrap()))
            .collect();
        self.allocations =
            allocate_all(&self.config.kernel, &tasks, &mut free)?;
        Ok(())
    }

    /// Returns a [Command] which invokes `cargo` without going through the
    /// `rustup` facades (which add significant overhead)
    fn cargo_cmd(&self) -> Command {
        Command::new(self.sysroot.join("bin").join("cargo"))
    }

    /// Runs `cargo clean` on a particular target
    fn cargo_clean(&self, crate_name: &str) -> Result<()> {
        println!("cleaning {}", crate_name);

        let mut cmd = self.cargo_cmd();
        cmd.arg("clean");
        cmd.arg("-p");
        cmd.arg(crate_name);
        cmd.arg("--release");
        cmd.arg("--target");
        cmd.arg(&self.config.target);

        let status = cmd
            .status()
            .context(format!("failed to cargo clean ({:?})", cmd))?;

        if !status.success() {
            bail!("command failed, see output for details");
        }

        Ok(())
    }

    /// Returns a set of paths to remap in RUSTFLAGS
    fn remap_paths() -> Result<BTreeMap<PathBuf, &'static str>> {
        // Panic messages in crates have a long prefix; we'll shorten it using
        // the --remap-path-prefix argument to reduce message size.  We'll
        // remap local (Hubris) crates to /hubris, crates.io to /crates.io, and
        // git dependencies to /git
        let mut remap_paths = BTreeMap::new();

        // On Windows, std::fs::canonicalize returns a UNC path, i.e. one
        // beginning with "\\hostname\".  However, rustc expects a non-UNC
        // path for its --remap-path-prefix argument, so we use
        // `dunce::canonicalize` instead
        let cargo_home = dunce::canonicalize(std::env::var("CARGO_HOME")?)?;
        let mut cargo_git = cargo_home.clone();
        cargo_git.push("git");
        cargo_git.push("checkouts");
        remap_paths.insert(cargo_git, "/git");

        // This hash is canonical-ish: Cargo tries hard not to change it
        // https://github.com/rust-lang/cargo/blob/master/src/cargo/core/source/source_id.rs#L607-L630
        //
        // It depends on system architecture, so this won't work on (for example)
        // a Raspberry Pi, but the only downside is that panic messages will
        // be longer.
        let mut cargo_registry = cargo_home;
        cargo_registry.push("registry");
        cargo_registry.push("src");
        cargo_registry.push("github.com-1ecc6299db9ec823");
        remap_paths.insert(cargo_registry, "/crates.io");

        let mut hubris_dir =
            dunce::canonicalize(std::env::var("CARGO_MANIFEST_DIR")?)?;
        hubris_dir.pop(); // Remove "build/xtask"
        hubris_dir.pop();
        remap_paths.insert(hubris_dir, "/hubris");
        Ok(remap_paths)
    }

    /// Returns shared symbols defined by the bootloader, or an empty
    /// slice if none are present.
    fn shared_syms(&self) -> &[String] {
        if let Some(bootloader) = self.config.bootloader.as_ref() {
            &bootloader.sharedsyms
        } else {
            &[]
        }
    }

    /// Looks up the given memory region from the config file, returning
    /// a start..end range.  This function is allocation-unaware, i.e.
    /// it always returns the full range, regardless of what tasks have
    /// been mapped into it.
    fn get_memory(&self, name: &str) -> Result<Range<u32>> {
        let out = match self.config.outputs.get(name) {
            Some(o) => o,
            None => bail!("Could not find region {}", name),
        };
        if let Some(end) = out.address.checked_add(out.size) {
            Ok(out.address..end)
        } else {
            bail!(
                "output {}: address {:08x} size {:x} would overflow",
                name,
                out.address,
                out.size
            );
        }
    }

    /// Build the bootloader, if one is present. Builds `target/table.ld`,
    /// which is either empty (if no bootloader is present) or contains
    /// addresses of symbols for the non-secure application to call
    /// into the secure application.
    fn build_bootloader(&self) -> Result<()> {
        if self.config.bootloader.is_none() {
            File::create(self.out_file("table.ld"))
                .context("Could not create table.ld")?;
            return Ok(());
        }

        let bootloader = self.config.bootloader.as_ref().unwrap();

        let mut bootloader_memory = IndexMap::new();
        let flash = self.get_memory("bootloader_flash")?;
        let ram = self.get_memory("bootloader_ram")?;
        let sram = self.get_memory("bootloader_sram")?;
        let image_flash = if let Some(end) = bootloader
            .imagea_flash_start
            .checked_add(bootloader.imagea_flash_size)
        {
            bootloader.imagea_flash_start..end
        } else {
            bail!("image flash size is incorrect");
        };
        let image_ram = if let Some(end) = bootloader
            .imagea_ram_start
            .checked_add(bootloader.imagea_ram_size)
        {
            bootloader.imagea_ram_start..end
        } else {
            bail!("image ram size is incorrect");
        };

        bootloader_memory.insert(String::from("FLASH"), flash.clone());
        bootloader_memory.insert(String::from("RAM"), ram.clone());
        bootloader_memory.insert(String::from("SRAM"), sram.clone());
        bootloader_memory
            .insert(String::from("IMAGEA_FLASH"), image_flash.clone());
        bootloader_memory.insert(String::from("IMAGEA_RAM"), image_ram.clone());

        // The kernel is always placed first in FLASH
        let kernel_start = self.get_memory("flash")?.start;

        if kernel_start != bootloader_memory["FLASH"].end {
            bail!("mismatch between bootloader end and hubris start! check app.toml!");
        }

        generate_bootloader_linker_script(
            self.out_file("memory.x"),
            &bootloader_memory,
            Some(&bootloader.sections),
            &bootloader.sharedsyms,
        )?;

        fs::copy("build/kernel-link.x", "target/link.x")
            .context("Could not copy kernel-link.x")?;

        self.build(
            "bootloader", // TODO?
            &bootloader.name,
            &bootloader.features,
            BuildConfig {
                with_task_names: false,
                with_secure_separation: false,
                with_shared_syms: true,
            },
            &[],
        )?;

        // Need a bootloader binary for signing
        objcopy_translate_format(
            "elf32-littlearm",
            &self.out_file(&bootloader.name),
            "binary",
            &self.out_file("bootloader.bin"),
        )?;

        if let Some(signing) = self.config.signing.get("bootloader") {
            self.do_sign_file(signing, "bootloader", 0)?;
        }

        // We need to get the absolute symbols for the non-secure application
        // to call into the secure application. The easiest approach right now
        // is to generate the table in a separate section, objcopy just that
        // section and then re-insert those bits into the application section
        // via linker.

        objcopy_grab_binary(
            "elf32-littlearm",
            &self.out_file(&bootloader.name),
            &self.out_file("addr_blob.bin"),
        )?;

        let bytes = std::fs::read(&self.out_file("addr_blob.bin"))
            .context("Could not read addr_blob.bin")?;
        let mut linkscr = File::create(self.out_file("table.ld"))
            .context("Could not create table.ld")?;

        for b in bytes {
            writeln!(linkscr, "BYTE(0x{:x})", b)?;
        }
        Ok(())
    }

    /// Compiles a single task based on its task name in `app.toml`
    fn build_static_task(&self, task_name: &str) -> Result<()> {
        let task_toml = &self.config.tasks[task_name];
        self.build(
            task_name,
            &task_toml.name,
            &task_toml.features,
            BuildConfig {
                with_task_names: true,
                with_secure_separation: true,
                with_shared_syms: true,
            },
            &[],
        )
        .context(format!("failed to build {}", task_name))?;

        // Copy from Cargo's normal output directory into our target-specific
        // build folder.
        let mut lib_path = Path::new("target").join(&self.config.target);
        lib_path.push("release");
        let lib_name = format!("lib{}.a", task_toml.name.replace("-", "_"));
        lib_path.push(&lib_name);
        std::fs::copy(lib_path, self.out_file(&format!("{}.a", task_name)))
            .context(format!("Could not copy {}", lib_name))?;

        Ok(())
    }

    fn resolve_task_slots(&self, task_name: &str) -> Result<()> {
        use scroll::{Pread, Pwrite};

        let task_toml = &self.config.tasks[task_name];

        let in_task_bin = std::fs::read(self.out_file(task_name))?;
        let elf = goblin::elf::Elf::parse(&in_task_bin)?;

        let mut out_task_bin = in_task_bin.clone();

        for entry in task_slot::get_task_slot_table_entries(&in_task_bin, &elf)?
        {
            let in_task_idx = in_task_bin.pread_with::<u16>(
                entry.taskidx_file_offset as usize,
                elf::get_endianness(&elf),
            )?;

            let target_task_name = match task_toml.task_slots.get(entry.slot_name) {
            Some(x) => x,
            _ => bail!(
                "Program for task '{}' contains a task_slot named '{}', but it is missing from the app.toml",
                task_name,
                entry.slot_name
            ),
        };

            let target_task_idx =
            match self.config.tasks.get_index_of(target_task_name) {
                Some(x) => x,
                _ => bail!(
                    "app.toml sets task '{}' task_slot '{}' to task '{}', but no such task exists in the app.toml",
                    task_name,
                    entry.slot_name,
                    target_task_name
                ),
            };

            out_task_bin.pwrite_with::<u16>(
                target_task_idx as u16,
                entry.taskidx_file_offset as usize,
                elf::get_endianness(&elf),
            )?;

            if self.verbose {
                println!(
                "Task '{}' task_slot '{}' changed from task index 0x{:x} to task index 0x{:x}",
                task_name, entry.slot_name, in_task_idx, target_task_idx
            );
            }
        }

        Ok(std::fs::write(self.out_file(task_name), out_task_bin)?)
    }

    /// Builds the environment variables for a particular build command
    fn build_environment(
        &self,
        task_name: &str,
        options: BuildConfig,
        extra_env: &[(&'static str, &str)],
    ) -> Result<IndexMap<&'static str, String>> {
        // This works because we control the environment in which we're about
        // to invoke cargo, and never modify CARGO_TARGET in that environment.
        let remap_path_prefix: String = Self::remap_paths()?
            .iter()
            .map(|r| format!(" --remap-path-prefix={}={}", r.0.display(), r.1))
            .collect();

        let mut env = IndexMap::new();

        // Note that we insert the linker arguments even when building
        // staticlib targets (which don't use the linker at all).  This is
        // because RUSTFLAGS is part of the fingerprint that Cargo uses to
        // determine whether to rebuild.  If we built staticlibs without
        // linker flags, we would end up rebuilding downstream crates
        // (e.g. stm32h7), because we've still got a few non-staticlib
        // targets (the bootloader and the kernel).
        env.insert(
            "RUSTFLAGS",
            format!(
                "-C link-arg=-Tlink.x \
                 -L {} \
                 -C link-arg=-z -C link-arg=common-page-size=0x20 \
                 -C link-arg=-z -C link-arg=max-page-size=0x20 \
                 -C llvm-args=--enable-machine-outliner=never \
                 -C overflow-checks=y \
                 {}",
                self.out_dir().to_str().unwrap(),
                remap_path_prefix,
            ),
        );

        if options.with_task_names {
            let task_names: Vec<String> =
                self.config.tasks.keys().cloned().collect();
            env.insert("HUBRIS_TASKS", task_names.join(","));
        }

        // We allow for task- and app-specific configuration to be passed
        // via environment variables to build.rs scripts that may choose to
        // incorporate configuration into compilation.
        if let Some(task_config) = &self
            .config
            .tasks
            .get(task_name)
            .and_then(|t| t.config.as_ref())
        {
            env.insert(
                "HUBRIS_TASK_CONFIG",
                toml::to_string(&task_config).unwrap(),
            );
        }

        env.insert("HUBRIS_BOARD", self.config.board.clone());

        // secure_separation indicates that we have TrustZone enabled.
        // When TrustZone is enabled, the bootloader is secure and Hubris is
        // not secure.
        // When TrustZone is not enabled, both the bootloader and Hubris are
        // secure.
        if options.with_secure_separation {
            env.insert(
                "HUBRIS_SECURE",
                String::from(match self.config.secure_separation {
                    Some(s) if s => "0",
                    _ => "1",
                }),
            );
        }

        if options.with_shared_syms {
            let s = self.shared_syms();
            if !s.is_empty() {
                env.insert("SHARED_SYMS", s.join(","));
            }
        }

        for (k, v) in extra_env {
            env.insert(k, v.to_string());
        }

        if let Some(app_config) = &self.config.config {
            let app_cfg = toml::to_string(&app_config).unwrap();
            env.insert("HUBRIS_APP_CONFIG", app_cfg);
        }

        Ok(env)
    }

    //
    fn build(
        &self,
        task_name: &str,
        crate_name: &str,
        features: &[String],
        config: BuildConfig,
        extra_env: &[(&'static str, &str)],
    ) -> Result<()> {
        println!("building {}", crate_name);
        if self.edges {
            let mut tree = self.cargo_cmd();
            tree.arg("tree")
                .arg("--no-default-features")
                .arg("--edges")
                .arg("features")
                .arg("--verbose")
                .arg("-p")
                .arg(crate_name);
            if !features.is_empty() {
                tree.arg("--features");
                tree.arg(features.join(","));
            }
            println!("Running cargo {:?}", tree);
            let tree_status = tree
                .status()
                .context(format!("failed to run edge ({:?})", tree))?;
            if !tree_status.success() {
                bail!("tree command failed, see output for details");
            }
        }

        let mut cmd = self.cargo_cmd();
        cmd.arg("build")
            .arg("--release")
            .arg("--no-default-features")
            .arg("--target")
            .arg(&self.config.target)
            .arg("-p")
            .arg(crate_name);

        if self.verbose {
            cmd.arg("-v");
        }
        if !features.is_empty() {
            cmd.arg("--features");
            cmd.arg(features.join(","));
        }

        // Construct the build environment for this particular task
        for (var, value) in
            self.build_environment(task_name, config, extra_env)?
        {
            cmd.env(var, value);
        }

        let status = cmd
            .status()
            .context(format!("failed to run rustc ({:?})", cmd))?;

        if !status.success() {
            bail!("command failed, see output for details");
        }
        Ok(())
    }

    /// Links the given task, storing its output data in [all_output_sections]
    /// and its entry point in [ep]
    fn link_task(&mut self, task_name: &str) -> Result<()> {
        println!("linking task {}", task_name);

        let task_toml = &self.config.tasks[task_name];
        let task_requires = &self.task_requires[task_name];
        generate_task_linker_script(
            self.out_file("memory.x"),
            &self.allocations.tasks[task_name],
            Some(&task_toml.sections),
            task_toml
                .stacksize
                .or(self.config.stacksize)
                .ok_or_else(|| {
                    anyhow!(
                        "{}: no stack size specified and there is no default",
                        task_name
                    )
                })?,
        )
        .context(format!(
            "failed to generate linker script for {}",
            task_name
        ))?;

        fs::copy("build/task-link.x", self.out_file("link.x"))?;

        self.link(task_name)?;
        self.resolve_task_slots(task_name)?;

        // Load the task ELF file to collect its output sections and
        // entry point.
        let mut symbol_table = BTreeMap::default();
        let (ep, flash) = load_elf(
            &self.out_file(task_name),
            &mut self.all_output_sections,
            &mut symbol_table,
        )?;

        if flash > task_requires["flash"] as usize {
            bail!(
                "{} has insufficient flash: specified {} bytes, needs {}",
                task_toml.name,
                task_requires["flash"],
                flash
            );
        }

        self.entry_points.insert(task_name.to_owned(), ep);
        Ok(())
    }

    /// Links an archive (.a) file into an ELF file. This requires `link.x`
    /// to exist in the appropriate place (which therefore requires `table.ld`
    /// and `memory.x`)
    fn link(&self, bin_name: &str) -> Result<()> {
        let mut ld = self.sysroot.clone();
        for p in ["lib", "rustlib", &self.host_triple, "bin", "gcc-ld", "ld"] {
            ld.push(p);
        }
        let mut cmd = Command::new(ld);
        if self.verbose {
            cmd.arg("--verbose");
        }

        cmd.arg(format!("{}.a", bin_name));
        cmd.arg("-o");
        cmd.arg(bin_name);

        cmd.arg("-Tlink.x");
        cmd.arg("-z");
        cmd.arg("common-page-size=0x20");
        cmd.arg("-z");
        cmd.arg("max-page-size=0x20");
        cmd.arg("--gc-sections");
        cmd.arg("-m");
        cmd.arg("armelf"); // TODO: make this architecture-appropriate

        cmd.current_dir(self.out_dir());

        let status = cmd
            .status()
            .context(format!("failed to run linker ({:?})", cmd))?;

        if !status.success() {
            bail!("command failed, see output for details");
        }

        Ok(())
    }

    fn do_sign_file(
        &self,
        sign: &Signing,
        fname: &str,
        header_start: u32,
    ) -> Result<()> {
        if sign.method == "crc" {
            crc_image::update_crc(
                &self.out_file(&format!("{}.bin", fname)),
                &self.out_file(&format!("{}_crc.bin", fname)),
            )
        } else if sign.method == "rsa" {
            let priv_key = sign.priv_key.as_ref().unwrap();
            let root_cert = sign.root_cert.as_ref().unwrap();
            signed_image::sign_image(
                false, // TODO add an option to enable DICE
                &self.out_file(&format!("{}.bin", fname)),
                &self.cfg_dir().join(&priv_key),
                &self.cfg_dir().join(&root_cert),
                &self.out_file(&format!("{}_rsa.bin", fname)),
                &self.out_file("CMPA.bin"),
            )
        } else if sign.method == "ecc" {
            // Right now we just generate the header
            self.generate_ecc_header(
                &self.out_file("combined.bin"),
                &self.out_file("combined_ecc.bin"),
                header_start,
            )
        } else {
            bail!("Invalid sign method {}", sign.method);
        }
    }

    fn generate_ecc_header(
        &self,
        in_binary: &PathBuf,
        out_binary: &PathBuf,
        header_start: u32,
    ) -> Result<()> {
        use zerocopy::AsBytes;

        let mut bytes = std::fs::read(in_binary)?;
        let image_len = bytes.len();

        let flash = self.get_memory("flash")?;
        let ram = self.get_memory("ram")?;

        let header_byte_offset = (header_start - flash.start) as usize;

        let mut header: abi::ImageHeader = Default::default();

        header.magic = abi::HEADER_MAGIC;
        header.total_image_len = image_len as u32;

        header.sau_entries[0].rbar = flash.start;
        header.sau_entries[0].rlar = (flash.end - 1) & !0x1f | 1;

        header.sau_entries[1].rbar = ram.start;
        header.sau_entries[1].rlar = (ram.end - 1) & !0x1f | 1;

        // Our peripherals
        header.sau_entries[2].rbar = 0x4000_0000;
        header.sau_entries[2].rlar = 0x4fff_ffe0 | 1;

        header
            .write_to_prefix(&mut bytes[header_byte_offset..])
            .unwrap();

        let mut out = File::create(out_binary)?;
        out.write_all(&bytes)?;

        Ok(())
    }

    fn write_gdb_script(&self) -> Result<()> {
        let mut gdb_script = File::create(self.out_file("script.gdb"))?;
        writeln!(
            gdb_script,
            "add-symbol-file {}",
            self.out_file("kernel").to_slash().unwrap()
        )?;
        for name in self.config.tasks.keys() {
            writeln!(
                gdb_script,
                "add-symbol-file {}",
                self.out_file(name).to_slash().unwrap()
            )?;
        }
        if let Some(bootloader) = self.config.bootloader.as_ref() {
            writeln!(
                gdb_script,
                "add-symbol-file {}",
                self.out_file(&bootloader.name).to_slash().unwrap()
            )?;
        }
        for (path, remap) in Self::remap_paths()? {
            let mut path_str = path
                .to_str()
                .ok_or(anyhow!("Could not convert path{:?} to str", path))?
                .to_string();

            // Even on Windows, GDB expects path components to be separated by '/',
            // so we tweak the path here so that remapping works.
            if cfg!(windows) {
                path_str = path_str.replace("\\", "/");
            }
            writeln!(gdb_script, "set substitute-path {} {}", remap, path_str)?;
        }
        Ok(())
    }

    /// Builds and links the kernel
    fn build_kernel(&self) -> Result<()> {
        // Calculate the image ID by hashing all task output sections
        let mut image_id = fnv::FnvHasher::default();
        self.all_output_sections.hash(&mut image_id);
        let image_id = image_id.finish();

        // Format the descriptors for the kernel build.
        let kconfig = self.make_descriptors()?;
        let kconfig = ron::ser::to_string(&kconfig)?;

        // Link the kernel
        generate_kernel_linker_script(
            self.out_file("memory.x"),
            &self.allocations.kernel,
            self.config.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        )?;
        fs::copy("build/kernel-link.x", self.out_file("link.x"))
            .context("Could not copy kernel-link.x")?;

        // Build the kernel. The kernel is a [bin] target, so we don't need
        // to link it separately afterwards.
        self.build(
            "kernel",
            &self.config.kernel.name,
            &self.config.kernel.features,
            BuildConfig {
                with_task_names: false,
                with_secure_separation: true,
                with_shared_syms: false,
            },
            &[
                ("HUBRIS_KCONFIG", &kconfig),
                ("HUBRIS_IMAGE_ID", &format!("{}", image_id)),
            ],
        )?;

        let kern_path = Path::new("target")
            .join(&self.config.target)
            .join("release")
            .join(&self.config.kernel.name);
        std::fs::copy(&kern_path, self.out_file("kernel"))
            .context(format!("Could not copy {:?} to kernel", kern_path))?;

        Ok(())
    }

    /// Generate the application descriptor table that the kernel uses to find
    /// and start tasks.
    ///
    /// The layout of the table is a series of structs from the `abi` crate:
    ///
    /// - One `App` header.
    /// - Some number of `RegionDesc` records describing memory regions.
    /// - Some number of `TaskDesc` records describing tasks.
    /// - Some number of `Interrupt` records routing interrupts to tasks.
    fn make_descriptors(&self) -> Result<KernelConfig> {
        // Generate the three record sections concurrently.
        let mut regions = vec![];
        let mut task_descs = vec![];
        let mut irqs = vec![];

        // Region 0 is the NULL region, used as a placeholder. It gives no access to
        // memory.
        regions.push(abi::RegionDesc {
            base: 0,
            size: 32, // smallest legal size on ARMv7-M
            attributes: abi::RegionAttributes::empty(), // no rights
            reserved_zero: 0,
        });

        // Regions 1.. are the fixed peripheral regions, shared by tasks that
        // reference them. We'll build a lookup table so we can find them
        // efficiently by name later.
        let mut peripheral_index = IndexMap::new();

        // ARMv6-M and ARMv7-M require that memory regions be a power of two.
        // ARMv8-M does not.
        let power_of_two_required = match self.config.target.as_str() {
            "thumbv8m.main-none-eabihf" => false,
            "thumbv7em-none-eabihf" => true,
            "thumbv6m-none-eabi" => true,
            t => panic!("Unknown mpu requirements for target '{}'", t),
        };

        for (name, p) in self.config.peripherals.iter() {
            if power_of_two_required && !p.size.is_power_of_two() {
                panic!("Memory region for peripheral '{}' is required to be a power of two, but has size {}", name, p.size);
            }

            peripheral_index.insert(name, regions.len());

            // Peripherals are always mapped as Device + Read + Write.
            let attributes = abi::RegionAttributes::DEVICE
                | abi::RegionAttributes::READ
                | abi::RegionAttributes::WRITE;

            regions.push(abi::RegionDesc {
                base: p.address,
                size: p.size,
                attributes,
                reserved_zero: 0,
            });
        }

        for (name, p) in self.config.extratext.iter() {
            if power_of_two_required && !p.size.is_power_of_two() {
                panic!("Memory region for peripheral '{}' is required to be a power of two, but has size {}", name, p.size);
            }

            peripheral_index.insert(name, regions.len());

            // Extra text is marked as read/execute
            let attributes =
                abi::RegionAttributes::READ | abi::RegionAttributes::EXECUTE;

            regions.push(abi::RegionDesc {
                base: p.address,
                size: p.size,
                attributes,
                reserved_zero: 0,
            });
        }

        // The remaining regions are allocated to tasks on a first-come first-serve
        // basis.
        for (i, (name, task)) in self.config.tasks.iter().enumerate() {
            let requires = &self.task_requires[name];
            if power_of_two_required && !requires["flash"].is_power_of_two() {
                panic!("Flash for task '{}' is required to be a power of two, but has size {}", task.name, requires["flash"]);
            }

            if power_of_two_required && !requires["ram"].is_power_of_two() {
                panic!("Ram for task '{}' is required to be a power of two, but has size {}", task.name, requires["flash"]);
            }

            // Regions are referenced by index into the table we just generated.
            // Each task has up to 8, chosen from its 'requires' and 'uses' keys.
            let mut task_regions = [0; 8];

            if task.uses.len() + requires.len() > 8 {
                panic!(
                    "task {} uses {} peripherals and {} memories (too many)",
                    name,
                    task.uses.len(),
                    requires.len()
                );
            }

            // Generate a RegionDesc for each uniquely allocated memory region
            // referenced by this task, and install them as entries 0..N in the
            // task's region table.
            let allocs = &self.allocations.tasks[name];
            for (ri, (output_name, range)) in allocs.iter().enumerate() {
                let out = &self.config.outputs[output_name];
                let mut attributes = abi::RegionAttributes::empty();
                if out.read {
                    attributes |= abi::RegionAttributes::READ;
                }
                if out.write {
                    attributes |= abi::RegionAttributes::WRITE;
                }
                if out.execute {
                    attributes |= abi::RegionAttributes::EXECUTE;
                }
                if out.dma {
                    attributes |= abi::RegionAttributes::DMA;
                }
                // no option for setting DEVICE for this region

                task_regions[ri] = regions.len() as u8;

                regions.push(abi::RegionDesc {
                    base: range.start,
                    size: range.end - range.start,
                    attributes,
                    reserved_zero: 0,
                });
            }

            // For peripherals referenced by the task, we don't need to allocate
            // _new_ regions, since we did them all in advance. Just record the
            // entries for the TaskDesc.
            for (j, peripheral_name) in task.uses.iter().enumerate() {
                if let Some(&peripheral) =
                    peripheral_index.get(&peripheral_name)
                {
                    task_regions[allocs.len() + j] = peripheral as u8;
                } else {
                    bail!(
                    "Could not find peripheral `{}` referenced by task `{}`.",
                    peripheral_name,
                    name
                );
                }
            }

            let mut flags = abi::TaskFlags::empty();
            if task.start {
                flags |= abi::TaskFlags::START_AT_BOOT;
            }

            task_descs.push(abi::TaskDesc {
                regions: task_regions,
                entry_point: self.entry_points[name],
                initial_stack: self.allocations.tasks[name]["ram"].start
                    + task.stacksize.or(self.config.stacksize).unwrap(),
                priority: task.priority,
                flags,
            });

            // Interrupts.
            for (irq_str, &notification) in &task.interrupts {
                // The irq_str can be either a base-ten number, or a reference to a
                // peripheral. Distinguish them based on whether it parses as an
                // integer.
                match irq_str.parse::<u32>() {
                    Ok(irq_num) => {
                        // While it's possible to conceive of a world in which one
                        // might want to have a single interrupt set multiple
                        // notification bits, it's much easier to conceive of a
                        // world in which one has misunderstood that the second
                        // number in the interrupt tuple is in fact a mask, not an
                        // index.
                        if notification.count_ones() != 1 {
                            bail!(
                                "task {}: IRQ {}: notification mask (0b{:b}) \
                             has {} bits set (expected exactly one)",
                                name,
                                irq_str,
                                notification,
                                notification.count_ones()
                            );
                        }

                        irqs.push(abi::Interrupt {
                            irq: irq_num,
                            task: i as u32,
                            notification,
                        });
                    }
                    Err(_) => {
                        // This might be an error, or might be a peripheral
                        // reference.
                        //
                        // Peripheral references are of the form "P.I", where P is
                        // the peripheral name and I is the name of one of the
                        // peripheral's defined interrupts.
                        if let Some(dot_pos) =
                            irq_str.bytes().position(|b| b == b'.')
                        {
                            let (pname, iname) = irq_str.split_at(dot_pos);
                            let iname = &iname[1..];
                            let periph =
                                self.config.peripherals.get(pname).ok_or_else(
                                    || {
                                        anyhow!(
                                    "task {} IRQ {} references peripheral {}, \
                                 which does not exist.",
                                    name,
                                    irq_str,
                                    pname,
                                )
                                    },
                                )?;
                            let irq_num = periph
                                .interrupts
                                .get(iname)
                                .ok_or_else(|| {
                                    anyhow!(
                                    "task {} IRQ {} references interrupt {} \
                                 on peripheral {}, but that interrupt name \
                                 is not defined for that peripheral.",
                                    name,
                                    irq_str,
                                    iname,
                                    pname,
                                )
                                })?;
                            irqs.push(abi::Interrupt {
                                irq: *irq_num,
                                task: i as u32,
                                notification,
                            });
                        } else {
                            bail!(
                                "task {}: IRQ name {} does not match any \
                             known peripheral interrupt, and is not an \
                             integer.",
                                name,
                                irq_str,
                            );
                        }
                    }
                }
            }
        }

        let supervisor_notification = match &self.config.supervisor {
            Some(supervisor) => supervisor.notification,
            // TODO: this exists for back-compat with incredibly early Hubris,
            // we can likely remove it.
            None => 0,
        };
        Ok(KernelConfig {
            irqs,
            tasks: task_descs,
            regions,
            supervisor_notification,
        })
    }

    fn write_archive(&self) -> Result<()> {
        // Bundle everything up into an archive.
        let mut archive = Archive::new(
            self.out_file(&format!("build-{}.zip", self.config.name)),
        )?;

        archive.text(
            "README.TXT",
            "\
        This is a build archive containing firmware build artifacts.\n\n\
        - app.toml is the config file used to build the firmware.\n\
        - git-rev is the commit it was built from, with optional dirty flag.\n\
        - info/ contains human-readable data like logs.\n\
        - elf/ contains ELF images for all firmware components.\n\
        - elf/tasks/ contains each task by name.\n\
        - elf/kernel is the kernel.\n\
        - img/ contains the final firmware images.\n",
        )?;

        let (git_rev, git_dirty) = get_git_status()?;
        archive.text(
            "git-rev",
            format!("{}{}", git_rev, if git_dirty { "-dirty" } else { "" }),
        )?;
        archive.copy(&self.cfg_file, "app.toml")?;
        if let Some(chip) = &self.config.chip {
            let chip_file = self.cfg_dir().join(chip);
            let chip_filename =
                chip_file.file_name().unwrap().to_str().unwrap().to_owned();
            archive.copy(chip_file, chip_filename)?;
        }

        let elf_dir = PathBuf::from("elf");
        let tasks_dir = elf_dir.join("task");
        for name in self.config.tasks.keys() {
            archive.copy(self.out_file(name), tasks_dir.join(name))?;
        }
        archive.copy(self.out_file("kernel"), elf_dir.join("kernel"))?;

        let info_dir = PathBuf::from("info");
        archive.copy(
            self.out_file("allocations.txt"),
            info_dir.join("allocations.txt"),
        )?;
        archive.copy(self.out_file("map.txt"), info_dir.join("map.txt"))?;

        let img_dir = PathBuf::from("img");
        archive.copy(
            self.out_file("combined.srec"),
            img_dir.join("combined.srec"),
        )?;
        archive.copy(
            self.out_file("combined.elf"),
            img_dir.join("combined.elf"),
        )?;
        archive.copy(
            self.out_file("combined.ihex"),
            img_dir.join("combined.ihex"),
        )?;
        archive.copy(
            self.out_file("combined.bin"),
            img_dir.join("combined.bin"),
        )?;

        if let Some(bootloader) = self.config.bootloader.as_ref() {
            archive.copy(
                self.out_file(&bootloader.name),
                img_dir.join(&bootloader.name),
            )?;
        }
        for s in self.config.signing.keys() {
            let name = format!(
                "{}_{}.bin",
                s,
                self.config.signing.get(s).unwrap().method
            );
            archive.copy(self.out_file(&name), img_dir.join(&name))?;
        }

        archive
            .copy(self.out_file("final.srec"), img_dir.join("final.srec"))?;
        archive.copy(self.out_file("final.elf"), img_dir.join("final.elf"))?;
        archive
            .copy(self.out_file("final.ihex"), img_dir.join("final.ihex"))?;
        archive.copy(self.out_file("final.bin"), img_dir.join("final.bin"))?;

        //
        // To allow for the image to be flashed based only on the archive
        // (e.g., by Humility), we pull in our flash configuration, flatten it
        // to pull in any external configuration files, serialize it, and add
        // it to the archive.
        //
        let mut config = crate::flash::config(&self.config.board.as_str())?;
        config.flatten()?;

        archive.text(img_dir.join("flash.ron"), ron::to_string(&config)?)?;

        archive.finish()
    }

    fn build_final_image(&mut self) -> Result<()> {
        let mut ksymbol_table = BTreeMap::default();
        let (kentry, _) = load_elf(
            &self.out_file("kernel"),
            &mut self.all_output_sections,
            &mut ksymbol_table,
        )?;

        // Generate combined SREC, which is our source of truth for combined images.
        write_srec(
            &self.all_output_sections,
            kentry,
            &self.out_file("combined.srec"),
        )?;

        // Convert SREC to other formats for convenience.
        objcopy_translate_format(
            "srec",
            &self.out_file("combined.srec"),
            "elf32-littlearm",
            &self.out_file("combined.elf"),
        )?;
        objcopy_translate_format(
            "srec",
            &self.out_file("combined.srec"),
            "ihex",
            &self.out_file("combined.ihex"),
        )?;
        objcopy_translate_format(
            "srec",
            &self.out_file("combined.srec"),
            "binary",
            &self.out_file("combined.bin"),
        )?;

        if let Some(signing) = self.config.signing.get("combined") {
            match ksymbol_table.get("HEADER") {
                None =>  bail!("Didn't find header symbol -- does the image need a placeholder?"),
                Some(_) => ()
            };
            self.do_sign_file(
                signing,
                "combined",
                *ksymbol_table.get("__header_start").unwrap(),
            )?;
        }

        // Okay we now have signed hubris image and signed bootloader
        // Time to combine the two!
        if let Some(bootloader) = self.config.bootloader.as_ref() {
            let file_image = std::fs::read(&self.out_file(&bootloader.name))?;
            let elf = goblin::elf::Elf::parse(&file_image)?;

            let bootloader_entry = elf.header.e_entry as u32;

            let bootloader_fname =
                if let Some(signing) = self.config.signing.get("bootloader") {
                    format!("bootloader_{}.bin", signing.method)
                } else {
                    "bootloader.bin".into()
                };

            let hubris_fname =
                if let Some(signing) = self.config.signing.get("combined") {
                    format!("combined_{}.bin", signing.method)
                } else {
                    "combined.bin".into()
                };

            smash_bootloader(
                &self.out_file(&bootloader_fname),
                self.get_memory("bootloader_flash")?.start,
                &self.out_file(&hubris_fname),
                self.get_memory("flash")?.start,
                bootloader_entry,
                &self.out_file("final.srec"),
            )?;

            objcopy_translate_format(
                "srec",
                &self.out_file("final.srec"),
                "elf32-littlearm",
                &self.out_file("final.elf"),
            )?;

            objcopy_translate_format(
                "srec",
                &self.out_file("final.srec"),
                "ihex",
                &self.out_file("final.ihex"),
            )?;

            objcopy_translate_format(
                "srec",
                &self.out_file("final.srec"),
                "binary",
                &self.out_file("final.bin"),
            )?;
        } else {
            std::fs::copy(
                self.out_file("combined.srec"),
                self.out_file("final.srec"),
            )?;

            std::fs::copy(
                self.out_file("combined.elf"),
                self.out_file("final.elf"),
            )?;

            std::fs::copy(
                self.out_file("combined.ihex"),
                self.out_file("final.ihex"),
            )?;

            std::fs::copy(
                self.out_file("combined.bin"),
                self.out_file("final.bin"),
            )?;
        }
        Ok(())
    }

    fn write_util_files(&self) -> Result<()> {
        /* TODO
        for (name, new_range) in &memories {
            let orig_range = self.get_memory(name)?;
            let size = new_range.start - orig_range.start;
            let percent = size * 100 / (orig_range.end - orig_range.start);
            println!(
                "  {:<6} 0x{:x} ({}%)",
                format!("{}:", name),
                size,
                percent
            );
        }
        */

        let mut infofile = File::create(self.out_file("allocations.txt"))?;
        writeln!(infofile, "kernel: {:#x?}", self.allocations.kernel)?;
        writeln!(infofile, "tasks: {:#x?}", self.allocations.tasks)?;

        // Write a map file, because that seems nice.
        let mut mapfile = File::create(&self.out_file("map.txt"))?;
        writeln!(mapfile, "ADDRESS  END          SIZE FILE")?;
        for (base, sec) in &self.all_output_sections {
            let size = sec.data.len() as u32;
            let end = base + size;
            writeln!(
                mapfile,
                "{:08x} {:08x} {:>8x} {}",
                base,
                end,
                size,
                sec.source_file.display()
            )?;
        }
        Ok(())
    }
}

pub fn package(
    verbose: bool,
    edges: bool,
    cfg: &Path,
    tasks_to_build: Option<Vec<String>>,
) -> Result<()> {
    let mut worker = Packager::init(verbose, edges, cfg)?;
    worker.create_out_dir()?;

    // Run `cargo clean` if this is a rebuild with a new `app.toml`
    worker.check_rebuild()?;

    // If we're using filters, we change behavior at the end. Record this in a
    // convenient flag, which also checks that the partial build is valid.
    let partial_build = worker.check_partial_build(&tasks_to_build)?;

    // The bootloader must be built first, because some tasks may rely on
    // calling into secure symbols that it defines (in `target/table.ld`)
    if !partial_build {
        worker.build_bootloader()?;
    }

    for task_name in worker.config.tasks.keys() {
        // Only build the task if we're building the full image or the task
        // is present in the tasks_to_build list.
        if tasks_to_build
            .as_ref()
            .map(|i| i.contains(task_name))
            .unwrap_or(true)
        {
            worker.build_static_task(task_name)?;
        }
    }

    // If we've done a partial build, we can't do the rest because we're
    // missing required information, so, escape.
    if partial_build {
        return Ok(());
    }

    worker.allocate_all()?;
    let task_names = worker.config.tasks.keys().cloned().collect::<Vec<_>>();
    for task_name in &task_names {
        worker.link_task(task_name)?;
    }

    // The kernel has to be built after the tasks, because it needs their
    // entry points.
    worker.build_kernel()?;

    worker.write_util_files()?;

    worker.build_final_image()?;
    worker.write_gdb_script()?;
    worker.write_archive()?;

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////

fn smash_bootloader(
    bootloader: &PathBuf,
    bootloader_addr: u32,
    hubris: &PathBuf,
    hubris_addr: u32,
    entry: u32,
    out: &PathBuf,
) -> Result<()> {
    let mut srec_out = vec![];
    srec_out.push(srec::Record::S0("hubris+bootloader".to_string()));

    let bootloader = std::fs::read(bootloader)?;

    let mut addr = bootloader_addr;
    for chunk in bootloader.chunks(255 - 5) {
        srec_out.push(srec::Record::S3(srec::Data {
            address: srec::Address32(addr),
            data: chunk.to_vec(),
        }));
        addr += chunk.len() as u32;
    }

    drop(bootloader);

    let hubris = std::fs::read(hubris)?;

    let mut addr = hubris_addr;
    for chunk in hubris.chunks(255 - 5) {
        srec_out.push(srec::Record::S3(srec::Data {
            address: srec::Address32(addr),
            data: chunk.to_vec(),
        }));
        addr += chunk.len() as u32;
    }

    drop(hubris);

    let out_sec_count = srec_out.len() - 1; // header
    if out_sec_count < 0x1_00_00 {
        srec_out.push(srec::Record::S5(srec::Count16(out_sec_count as u16)));
    } else if out_sec_count < 0x1_00_00_00 {
        srec_out.push(srec::Record::S6(srec::Count24(out_sec_count as u32)));
    } else {
        panic!("SREC limit of 2^24 output sections exceeded");
    }

    srec_out.push(srec::Record::S7(srec::Address32(entry)));

    let srec_image = srec::writer::generate_srec_file(&srec_out);
    std::fs::write(out, srec_image)?;
    Ok(())
}

fn generate_bootloader_linker_script(
    path: PathBuf,
    map: &IndexMap<String, Range<u32>>,
    sections: Option<&IndexMap<String, String>>,
    sharedsyms: &[String],
) -> Result<()> {
    let mut linkscr = File::create(path)
        .context("Could not create bootloader link script")?;
    writeln!(linkscr, "MEMORY\n{{").unwrap();
    for (name, range) in map {
        let start = range.start;
        let end = range.end;
        let name = name.to_ascii_uppercase();
        writeln!(
            linkscr,
            "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
            name,
            start,
            end - start
        )
        .unwrap();
    }
    writeln!(linkscr, "}}").unwrap();

    // Mappings for the secure entry. This needs to live in flash and be
    // aligned to 32 bytes.
    if let Some(map) = sections {
        writeln!(linkscr, "SECTIONS {{").unwrap();

        for (section, memory) in map {
            writeln!(linkscr, "  .{} : ALIGN(32) {{", section).unwrap();
            writeln!(linkscr, "    __start_{} = .;", section).unwrap();
            writeln!(linkscr, "    KEEP(*(.{} .{}.*));", section, section)
                .unwrap();
            writeln!(linkscr, "     . = ALIGN(32);").unwrap();
            writeln!(linkscr, "    __end_{} = .;", section).unwrap();
            writeln!(linkscr, "    PROVIDE(address_of_start_{} = .);", section)
                .unwrap();
            writeln!(linkscr, "    LONG(__start_{});", section).unwrap();
            writeln!(linkscr, "    PROVIDE(address_of_end_{} = .);", section)
                .unwrap();
            writeln!(linkscr, "    LONG(__end_{});", section).unwrap();

            writeln!(linkscr, "  }} > {}", memory.to_ascii_uppercase())
                .unwrap();
        }
        writeln!(linkscr, "}} INSERT BEFORE .bss").unwrap();
    }

    // Symbol addresses to be exported to tasks. This gets stripped
    // later
    writeln!(linkscr, "SECTIONS {{").unwrap();
    writeln!(linkscr, "  .fake_output : ALIGN(32) {{").unwrap();

    for s in sharedsyms {
        writeln!(linkscr, "    LONG({});", s).unwrap();
    }
    writeln!(linkscr, "  }} > FLASH").unwrap();

    writeln!(linkscr, "  .symbol_defs : {{").unwrap();

    writeln!(linkscr, "  PROVIDE(address_of_imagea_flash = .);").unwrap();
    writeln!(linkscr, "  LONG(ORIGIN(IMAGEA_FLASH));").unwrap();
    writeln!(linkscr, "  PROVIDE(address_of_imagea_ram = .);").unwrap();
    writeln!(linkscr, "  LONG(ORIGIN(IMAGEA_RAM));").unwrap();
    writeln!(linkscr, "  PROVIDE(address_of_test_region = .);").unwrap();
    writeln!(
        linkscr,
        "  LONG(ORIGIN(IMAGEA_FLASH) + LENGTH(IMAGEA_FLASH));"
    )
    .unwrap();
    writeln!(linkscr, "  }} > FLASH").unwrap();

    writeln!(linkscr, "}} INSERT BEFORE .bss").unwrap();

    writeln!(linkscr, "SECTIONS {{").unwrap();
    writeln!(linkscr, "  .attest (NOLOAD) : {{").unwrap();
    writeln!(linkscr, "  KEEP(*(.attestation .attestation.*))").unwrap();
    writeln!(linkscr, "  }} > SRAM").unwrap();
    writeln!(linkscr, "}} INSERT AFTER .uninit").unwrap();

    writeln!(linkscr, "IMAGEA = ORIGIN(IMAGEA_FLASH);").unwrap();
    Ok(())
}

fn generate_task_linker_script(
    path: PathBuf,
    map: &BTreeMap<String, Range<u32>>,
    sections: Option<&IndexMap<String, String>>,
    stacksize: u32,
) -> Result<()> {
    // Put the linker script somewhere the linker can find it
    let mut linkscr = File::create(path)?;

    fn emit(linkscr: &mut File, sec: &str, o: u32, l: u32) -> Result<()> {
        writeln!(
            linkscr,
            "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
            sec, o, l
        )?;
        Ok(())
    }

    writeln!(linkscr, "MEMORY\n{{")?;
    for (name, range) in map {
        let mut start = range.start;
        let end = range.end;
        let name = name.to_ascii_uppercase();

        // Our stack comes out of RAM
        if name == "RAM" {
            if stacksize & 0x7 != 0 {
                // If we are not 8-byte aligned, the kernel will not be
                // pleased -- and can't be blamed for a little rudeness;
                // check this here and fail explicitly if it's unaligned.
                bail!("specified stack size is not 8-byte aligned");
            }

            emit(&mut linkscr, "STACK", start, stacksize)?;
            start += stacksize;

            if start > end {
                bail!("specified stack size is greater than RAM size");
            }
        }

        emit(&mut linkscr, &name, start, end - start)?;
    }
    writeln!(linkscr, "}}")?;

    // The task may have defined additional section-to-memory mappings.
    if let Some(map) = sections {
        writeln!(linkscr, "SECTIONS {{")?;
        for (section, memory) in map {
            writeln!(linkscr, "  .{} (NOLOAD) : ALIGN(4) {{", section)?;
            writeln!(linkscr, "    *(.{} .{}.*);", section, section)?;
            writeln!(linkscr, "  }} > {}", memory.to_ascii_uppercase())?;
        }
        writeln!(linkscr, "}} INSERT AFTER .uninit")?;
    }

    Ok(())
}

fn generate_kernel_linker_script(
    path: PathBuf,
    map: &BTreeMap<String, Range<u32>>,
    stacksize: u32,
) -> Result<()> {
    // Put the linker script somewhere the linker can find it
    let mut linkscr =
        File::create(path).context("Could not create kernel linker script")?;

    let mut stack_start = None;
    let mut stack_base = None;

    writeln!(linkscr, "MEMORY\n{{").unwrap();
    for (name, range) in map {
        let mut start = range.start;
        let end = range.end;
        let name = name.to_ascii_uppercase();

        // Our stack comes out of RAM
        if name == "RAM" {
            if stacksize & 0x7 != 0 {
                // If we are not 8-byte aligned, the kernel will not be
                // pleased -- and can't be blamed for a little rudeness;
                // check this here and fail explicitly if it's unaligned.
                bail!("specified kernel stack size is not 8-byte aligned");
            }

            stack_base = Some(start);
            writeln!(
                linkscr,
                "STACK (rw) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
                start, stacksize,
            )?;
            start += stacksize;
            stack_start = Some(start);

            if start > end {
                bail!("specified kernel stack size is greater than RAM size");
            }
        }

        writeln!(
            linkscr,
            "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
            name,
            start,
            end - start
        )
        .unwrap();
    }
    writeln!(linkscr, "}}").unwrap();
    writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
    writeln!(linkscr, "_stack_base = 0x{:08x};", stack_base.unwrap()).unwrap();
    writeln!(linkscr, "_stack_start = 0x{:08x};", stack_start.unwrap())
        .unwrap();

    Ok(())
}

#[derive(Debug, Clone, Default)]
struct Allocations {
    /// Map from memory-name to address-range
    kernel: BTreeMap<String, Range<u32>>,
    /// Map from task-name to memory-name to address-range
    tasks: BTreeMap<String, BTreeMap<String, Range<u32>>>,
}

/// Allocates address space from all regions for the kernel and all tasks.
///
/// The allocation strategy is slightly involved, because of the limitations of
/// the ARMv7-M MPU. (Currently we use the same strategy on ARMv8-M even though
/// it's more flexible.)
///
/// Address space regions are required to be power-of-two in size and naturally
/// aligned. In other words, all the addresses in a single region must have some
/// number of top bits the same, and any combination of bottom bits.
///
/// To complicate things,
///
/// - There's no particular reason why the memory regions defined in the
///   app.toml need to be aligned to any particular power of two.
/// - When there's a bootloader added to the image, it will bump a nicely
///   aligned starting address forward by a few kiB anyway.
/// - Said bootloader requires the kernel text to appear immediately after it in
///   ROM, so, the kernel must be laid down first. (This is not true of RAM, but
///   putting the kernel first in RAM has some useful benefits.)
///
/// The method we're using here is essentially the "deallocate" side of a
/// power-of-two buddy allocator, only simplified because we're using it to
/// allocate a series of known sizes.
///
/// To allocate space for a single request, we
///
/// - Check the alignment of the current position pointer.
/// - Find the largest pending request of that alignment _or less._
/// - If we found one, bump the pointer forward and repeat.
/// - If not, find the smallest pending request that requires greater alignment,
///   and skip address space until it can be satisfied, and then repeat.
///
/// This means that the algorithm needs to keep track of a queue of pending
/// requests per alignment size.
fn allocate_all(
    kernel: &crate::Kernel,
    tasks: &IndexMap<String, crate::SizedTask>,
    free: &mut IndexMap<String, Range<u32>>,
) -> Result<Allocations> {
    // Collect all allocation requests into queues, one per memory type, indexed
    // by allocation size. This is equivalent to required alignment because of
    // the naturally-aligned-power-of-two requirement.
    //
    // We keep kernel and task requests separate so we can always service the
    // kernel first.
    //
    // The task map is: memory name -> allocation size -> queue of task name.
    // The kernel map is: memory name -> allocation size
    let kernel_requests = &kernel.requires;

    let mut task_requests: BTreeMap<&str, BTreeMap<u32, VecDeque<&str>>> =
        BTreeMap::new();

    for (name, SizedTask { requires, task }) in tasks {
        for (mem, &amt) in requires {
            if !amt.is_power_of_two() {
                bail!("task {}, memory region {}: requirement {} is not a power of two.",
                    task.name, name, amt);
            }
            task_requests
                .entry(mem.as_str())
                .or_default()
                .entry(amt)
                .or_default()
                .push_back(name.as_str());
        }
    }

    // Okay! Do memory types one by one, fitting kernel first.
    let mut allocs = Allocations::default();
    for (region, avail) in free {
        let mut k_req = kernel_requests.get(region.as_str());
        let mut t_reqs = task_requests.get_mut(region.as_str());

        fn reqs_map_not_empty(
            om: &Option<&mut BTreeMap<u32, VecDeque<&str>>>,
        ) -> bool {
            om.iter()
                .flat_map(|map| map.values())
                .any(|q| !q.is_empty())
        }

        'fitloop: while k_req.is_some() || reqs_map_not_empty(&t_reqs) {
            let align = if avail.start == 0 {
                // Lie to keep the masks in range. This could be avoided by
                // tracking log2 of masks rather than masks.
                1 << 31
            } else {
                1 << avail.start.trailing_zeros()
            };

            // Search order is:
            // - Kernel.
            // - Task requests equal to or smaller than this alignment, in
            //   descending order of size.
            // - Task requests larger than this alignment, in ascending order of
            //   size.

            if let Some(&sz) = k_req.take() {
                // The kernel wants in on this.
                allocs
                    .kernel
                    .insert(region.to_string(), allocate_k(region, sz, avail)?);
                continue 'fitloop;
            }

            if let Some(t_reqs) = t_reqs.as_mut() {
                for (&sz, q) in t_reqs.range_mut(..=align).rev() {
                    if let Some(task) = q.pop_front() {
                        // We can pack an equal or smaller one in.
                        allocs
                            .tasks
                            .entry(task.to_string())
                            .or_default()
                            .insert(
                                region.to_string(),
                                allocate_one(region, sz, avail)?,
                            );
                        continue 'fitloop;
                    }
                }

                for (&sz, q) in t_reqs.range_mut(align + 1..) {
                    if let Some(task) = q.pop_front() {
                        // We've gotta use a larger one.
                        allocs
                            .tasks
                            .entry(task.to_string())
                            .or_default()
                            .insert(
                                region.to_string(),
                                allocate_one(region, sz, avail)?,
                            );
                        continue 'fitloop;
                    }
                }
            }

            // If we reach this point, it means our loop condition is wrong,
            // because one of the above things should really have happened.
            // Panic here because otherwise it's a hang.
            panic!("loop iteration without progess made!");
        }
    }

    Ok(allocs)
}

fn allocate_k(
    region: &str,
    size: u32,
    avail: &mut Range<u32>,
) -> Result<Range<u32>> {
    // Our base address will be larger than avail.start if it doesn't meet our
    // minimum requirements. Round up.
    let base = (avail.start + 15) & !15;

    if !avail.contains(&(base + size - 1)) {
        bail!(
            "out of {}: can't allocate {} more after base {:x}",
            region,
            size,
            base
        )
    }

    let end = base + size;
    // Update the available range to exclude what we've taken.
    avail.start = end;

    Ok(base..end)
}

fn allocate_one(
    region: &str,
    size: u32,
    avail: &mut Range<u32>,
) -> Result<Range<u32>> {
    // This condition is ensured by allocate_all.
    assert!(size.is_power_of_two());

    let size_mask = size - 1;

    // Our base address will be larger than avail.start if it doesn't meet our
    // minimum requirements. Round up.
    let base = (avail.start + size_mask) & !size_mask;

    if base >= avail.end || size > avail.end - base {
        bail!(
            "out of {}: can't allocate {} more after base {:x}",
            region,
            size,
            base
        )
    }

    let end = base + size;
    // Update the available range to exclude what we've taken.
    avail.start = end;

    Ok(base..end)
}

#[derive(Serialize)]
struct KernelConfig {
    tasks: Vec<abi::TaskDesc>,
    regions: Vec<abi::RegionDesc>,
    irqs: Vec<abi::Interrupt>,
    supervisor_notification: u32,
}

/// Loads an SREC file into the same representation we use for ELF. This is
/// currently unused, but I'm keeping it compiling as proof that it's possible,
/// because we may need it later.
#[allow(dead_code)]
fn load_srec(
    input: &Path,
    output: &mut BTreeMap<u32, LoadSegment>,
) -> Result<u32> {
    let srec_text = std::fs::read_to_string(input)?;
    for record in srec::reader::read_records(&srec_text) {
        let record = record?;
        match record {
            srec::Record::S3(data) => {
                // Check for address overlap
                let range =
                    data.address.0..data.address.0 + data.data.len() as u32;
                if let Some(overlap) = output.range(range.clone()).next() {
                    bail!(
                        "{}: record address range {:x?} overlaps {:x}",
                        input.display(),
                        range,
                        overlap.0
                    )
                }
                output.insert(
                    data.address.0,
                    LoadSegment {
                        source_file: input.into(),
                        data: data.data,
                    },
                );
            }
            srec::Record::S7(srec::Address32(e)) => return Ok(e),
            _ => (),
        }
    }

    panic!("SREC file missing terminating S7 record");
}

fn load_elf(
    input: &Path,
    output: &mut BTreeMap<u32, LoadSegment>,
    symbol_table: &mut BTreeMap<String, u32>,
) -> Result<(u32, usize)> {
    use goblin::container::Container;
    use goblin::elf::program_header::PT_LOAD;

    let file_image = std::fs::read(input)?;
    let elf = goblin::elf::Elf::parse(&file_image)?;

    if elf.header.container()? != Container::Little {
        bail!("where did you get a big-endian image?");
    }
    if elf.header.e_machine != goblin::elf::header::EM_ARM {
        bail!("this is not an ARM file");
    }

    let mut flash = 0;

    // Good enough.
    for phdr in &elf.program_headers {
        // Skip sections that aren't intended to be loaded.
        if phdr.p_type != PT_LOAD {
            continue;
        }
        let offset = phdr.p_offset as usize;
        let size = phdr.p_filesz as usize;
        // Note that we are using Physical, i.e. LOADADDR, rather than virtual.
        // This distinction is important for things like the rodata image, which
        // is loaded in flash but expected to be copied to RAM.
        let addr = phdr.p_paddr as u32;

        flash += size;

        // Check for address overlap
        let range = addr..addr + size as u32;
        if let Some(overlap) = output.range(range.clone()).next() {
            if overlap.1.source_file != input {
                bail!(
                    "{}: address range {:x?} overlaps {:x} \
                    (from {}); does {} have an insufficient amount of flash?",
                    input.display(),
                    range,
                    overlap.0,
                    overlap.1.source_file.display(),
                    input.display(),
                );
            } else {
                bail!(
                    "{}: ELF file internally inconsistent: \
                    address range {:x?} overlaps {:x}",
                    input.display(),
                    range,
                    overlap.0,
                );
            }
        }
        output.insert(
            addr,
            LoadSegment {
                source_file: input.into(),
                data: file_image[offset..offset + size].to_vec(),
            },
        );
    }

    for s in elf.syms.iter() {
        let index = s.st_name;

        if let Some(name) = elf.strtab.get_at(index) {
            symbol_table.insert(name.to_string(), s.st_value as u32);
        }
    }

    // Return both our entry and the total allocated flash, allowing the
    // caller to assure that the allocated flash does not exceed the task's
    // required flash
    Ok((elf.header.e_entry as u32, flash))
}

/// Keeps track of a build archive being constructed.
struct Archive {
    /// Place where we'll put the final zip file.
    final_path: PathBuf,
    /// Name of temporary file used during construction.
    tmp_path: PathBuf,
    /// ZIP output to the temporary file.
    inner: zip::ZipWriter<File>,
    /// Options used for every file.
    opts: zip::write::FileOptions,
}

impl Archive {
    /// Creates a new build archive that will, when finished, be placed at
    /// `dest`.
    fn new(dest: impl AsRef<Path>) -> Result<Self> {
        let final_path = PathBuf::from(dest.as_ref());

        let mut tmp_path = final_path.clone();
        tmp_path.set_extension("zip.partial");

        let archive = File::create(&tmp_path)?;
        let mut inner = zip::ZipWriter::new(archive);
        inner.set_comment("hubris build archive v1.0.0");
        Ok(Self {
            final_path,
            tmp_path,
            inner,
            opts: zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Bzip2),
        })
    }

    /// Copies the file at `src_path` into the build archive at `zip_path`.
    fn copy(
        &mut self,
        src_path: impl AsRef<Path>,
        zip_path: impl AsRef<Path>,
    ) -> Result<()> {
        let mut input = File::open(src_path)?;
        self.inner
            .start_file_from_path(zip_path.as_ref(), self.opts)?;
        std::io::copy(&mut input, &mut self.inner)?;
        Ok(())
    }

    /// Creates a text file in the archive at `zip_path` with `contents`.
    fn text(
        &mut self,
        zip_path: impl AsRef<Path>,
        contents: impl AsRef<str>,
    ) -> Result<()> {
        self.inner
            .start_file_from_path(zip_path.as_ref(), self.opts)?;
        self.inner.write_all(contents.as_ref().as_bytes())?;
        Ok(())
    }

    /// Completes the archive and moves it to its intended location.
    ///
    /// If you drop an `Archive` without calling this, it will leave a temporary
    /// file rather than creating the final archive.
    fn finish(self) -> Result<()> {
        let Self {
            tmp_path,
            final_path,
            mut inner,
            ..
        } = self;
        inner.finish()?;
        drop(inner);
        std::fs::rename(tmp_path, final_path)?;
        Ok(())
    }
}

/// Gets the status of a git repository containing the current working
/// directory. Returns two values:
///
/// - A `String` containing the git commit hash.
/// - A `bool` indicating whether the repository has uncommitted changes.
fn get_git_status() -> Result<(String, bool)> {
    let mut cmd = Command::new("git");
    cmd.arg("rev-parse").arg("HEAD");
    let out = cmd.output()?;
    if !out.status.success() {
        bail!("git rev-parse failed");
    }
    let rev = std::str::from_utf8(&out.stdout)?.trim().to_string();

    let mut cmd = Command::new("git");
    cmd.arg("diff-index").arg("--quiet").arg("HEAD").arg("--");
    let status = cmd
        .status()
        .context(format!("failed to get git status ({:?})", cmd))?;

    Ok((rev, !status.success()))
}

fn write_srec(
    sections: &BTreeMap<u32, LoadSegment>,
    kentry: u32,
    out: &Path,
) -> Result<()> {
    let mut srec_out = vec![];
    srec_out.push(srec::Record::S0("hubris".to_string()));
    for (&base, sec) in sections {
        // SREC record size limit is 255 (0xFF). 32-bit addressed records
        // additionally contain a four-byte address and one-byte checksum, for a
        // payload limit of 255 - 5.
        let mut addr = base;
        for chunk in sec.data.chunks(255 - 5) {
            srec_out.push(srec::Record::S3(srec::Data {
                address: srec::Address32(addr),
                data: chunk.to_vec(),
            }));
            addr += chunk.len() as u32;
        }
    }
    let out_sec_count = srec_out.len() - 1; // header
    if out_sec_count < 0x1_00_00 {
        srec_out.push(srec::Record::S5(srec::Count16(out_sec_count as u16)));
    } else if out_sec_count < 0x1_00_00_00 {
        srec_out.push(srec::Record::S6(srec::Count24(out_sec_count as u32)));
    } else {
        panic!("SREC limit of 2^24 output sections exceeded");
    }

    srec_out.push(srec::Record::S7(srec::Address32(kentry)));

    let srec_image = srec::writer::generate_srec_file(&srec_out);
    std::fs::write(out, srec_image)?;
    Ok(())
}

fn objcopy_grab_binary(in_format: &str, src: &Path, dest: &Path) -> Result<()> {
    let mut cmd = Command::new("arm-none-eabi-objcopy");
    cmd.arg("-I")
        .arg(in_format)
        .arg("-O")
        .arg("binary")
        .arg("--only-section=.fake_output")
        .arg(src)
        .arg(dest);

    let status = cmd.status()?;
    if !status.success() {
        bail!("objcopy failed, see output for details");
    }
    Ok(())
}

fn objcopy_translate_format(
    in_format: &str,
    src: &Path,
    out_format: &str,
    dest: &Path,
) -> Result<()> {
    let mut cmd = Command::new("arm-none-eabi-objcopy");
    cmd.arg("-I")
        .arg(in_format)
        .arg("-O")
        .arg(out_format)
        .arg(src)
        .arg(dest);

    let status = cmd
        .status()
        .context(format!("failed to objcopy ({:?})", cmd))?;

    if !status.success() {
        bail!("objcopy failed, see output for details");
    }
    Ok(())
}
