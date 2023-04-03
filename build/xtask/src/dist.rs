// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use atty::Stream;
use colored::*;
use indexmap::IndexMap;
use multimap::MultiMap;
use path_slash::{PathBufExt, PathExt};
use zerocopy::AsBytes;

use crate::{
    config::{BuildConfig, CabooseConfig, Config, ConfigPatches},
    elf,
    sizes::load_task_size,
    task_slot,
};

/// In practice, applications with active interrupt activity tend to use about
/// 650 bytes of stack. Because kernel stack overflows are annoying, we've
/// padded that a bit.
pub const DEFAULT_KERNEL_STACK: u32 = 1024;

/// Humility will (gracefully) refuse to load an archive version that is later
/// than its defined version, so this version number should be be used to
/// enforce flag days across Hubris and Humility.  To increase this version,
/// be sure to *first* bump the corresponding `MAX_HUBRIS_VERSION` version in
/// Humility.  Integrate that change into Humlity and be sure that the job
/// that generates the Humility binary necessary for Hubris's CI has run.
/// Once that binary is in place, you should be able to bump this version
/// without breaking CI.
const HUBRIS_ARCHIVE_VERSION: u32 = 8;

/// `PackageConfig` contains a bundle of data that's commonly used when
/// building a full app image, grouped together to avoid passing a bunch
/// of individual arguments to functions.
///
/// It should be trivial to calculate and kept constant during the build;
/// mutable build information should be accumulated elsewhere.
pub struct PackageConfig {
    /// Path to the `app.toml` file being built
    ///
    /// If this app is built using inheritance, `app_toml_file` refers to the
    /// **root** TOML file (and patches are in `self.patches`)
    app_toml_file: PathBuf,

    /// Patches from TOML inheritance mechanism
    patches: Option<ConfigPatches>,

    /// Directory containing the `app.toml` file being built
    app_src_dir: PathBuf,

    /// Loaded configuration
    pub toml: Config,

    /// Add `-v` to various build commands
    verbose: bool,

    /// Run `cargo tree --edges` before compiling, to show dependencies
    edges: bool,

    /// Directory where the build artifacts are placed, in the form
    /// `target/$NAME/dist`.
    dist_dir: PathBuf,

    /// Sysroot of the relevant toolchain
    sysroot: PathBuf,

    /// Host triple, e.g. `aarch64-apple-darwin`
    host_triple: String,

    /// List of paths to be remapped by the compiler, to minimize strings in
    /// the resulting binaries.
    remap_paths: BTreeMap<PathBuf, &'static str>,

    /// A single value produced by hashing the various linker scripts. This
    /// allows us to force a rebuild when the linker scripts change, which
    /// is not normally tracked by `cargo build`.
    link_script_hash: u64,
}

impl PackageConfig {
    pub fn new(
        app_toml_file: &Path,
        verbose: bool,
        edges: bool,
    ) -> Result<Self> {
        let toml = Config::from_file(app_toml_file)?;
        let dist_dir = Path::new("target").join(&toml.name).join("dist");
        let app_src_dir = app_toml_file
            .parent()
            .ok_or_else(|| anyhow!("Could not get app toml directory"))?;

        let sysroot = Command::new("rustc")
            .arg("--print")
            .arg("sysroot")
            .output()?;
        if !sysroot.status.success() {
            bail!("Could not find execute rustc to get sysroot");
        }
        let sysroot =
            PathBuf::from(std::str::from_utf8(&sysroot.stdout)?.trim());

        let host = Command::new(sysroot.join("bin").join("rustc"))
            .arg("-vV")
            .output()?;
        if !host.status.success() {
            bail!("Could not execute rustc to get host");
        }
        let host_triple = std::str::from_utf8(&host.stdout)?
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or_else(|| anyhow!("Could not get host from rustc"))?
            .to_string();

        let mut extra_hash = fnv::FnvHasher::default();
        for f in ["task-link.x", "task-rlink.x", "kernel-link.x"] {
            let file_data = std::fs::read(Path::new("build").join(f))?;
            file_data.hash(&mut extra_hash);
        }

        Ok(Self {
            app_toml_file: toml.app_toml_path.to_path_buf(),
            patches: toml.patches.clone(),
            app_src_dir: app_src_dir.to_path_buf(),
            toml,
            verbose,
            edges,
            dist_dir,
            sysroot,
            host_triple,
            remap_paths: Self::remap_paths()?,
            link_script_hash: extra_hash.finish(),
        })
    }

    fn img_dir(&self, img_name: &str) -> PathBuf {
        self.dist_dir.join(img_name)
    }

    pub fn img_file(&self, name: impl AsRef<Path>, img_name: &str) -> PathBuf {
        self.img_dir(img_name).join(name)
    }

    pub fn dist_file(&self, name: impl AsRef<Path>) -> PathBuf {
        self.dist_dir.join(name)
    }

    fn remap_paths() -> Result<BTreeMap<PathBuf, &'static str>> {
        // Panic messages in crates have a long prefix; we'll shorten it using
        // the --remap-path-prefix argument to reduce message size.  We'll remap
        // local (Hubris) crates to /hubris, crates.io to /crates.io, and git
        // dependencies to /git
        let mut remap_paths = BTreeMap::new();

        // On Windows, std::fs::canonicalize returns a UNC path, i.e. one
        // beginning with "\\hostname\".  However, rustc expects a non-UNC
        // path for its --remap-path-prefix argument, so we use
        // `dunce::canonicalize` instead
        if let Ok(home) = std::env::var("CARGO_HOME") {
            let cargo_home = dunce::canonicalize(home)?;
            let cargo_git = cargo_home.join("git").join("checkouts");
            remap_paths.insert(cargo_git, "/git");

            // This hash is canonical-ish: Cargo tries hard not to change it
            // https://github.com/rust-lang/cargo/blob/master/src/cargo/core/source/source_id.rs#L607-L630
            //
            // It depends on system architecture, so this won't work on (for example)
            // a Raspberry Pi, but the only downside is that panic messages will
            // be longer.
            let cargo_registry = cargo_home
                .join("registry")
                .join("src")
                .join("github.com-1ecc6299db9ec823");
            remap_paths.insert(cargo_registry, "/crates.io");
        }

        if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let mut hubris_dir = dunce::canonicalize(dir)?;
            hubris_dir.pop();
            hubris_dir.pop();
            remap_paths.insert(hubris_dir.to_path_buf(), "/hubris");
        }
        Ok(remap_paths)
    }
}

pub fn list_tasks(app_toml: &Path) -> Result<()> {
    let toml = Config::from_file(app_toml)?;
    let pad = toml
        .tasks
        .keys()
        .map(String::as_str)
        .chain(std::iter::once("kernel"))
        .map(|m| m.len())
        .max()
        .unwrap_or(1);
    println!("  {:<pad$}  CRATE", "TASK", pad = pad);
    println!("  {:<pad$}  {}", "kernel", toml.kernel.name, pad = pad);
    for (name, task) in toml.tasks {
        println!("  {:<pad$}  {}", name, task.name, pad = pad);
    }
    Ok(())
}

/// Represents allocations and free spaces for a particular image
type AllocationMap = (Allocations, IndexMap<String, Range<u32>>);

pub fn package(
    verbose: bool,
    edges: bool,
    app_toml: &Path,
    tasks_to_build: Option<Vec<String>>,
    dirty_ok: bool,
) -> Result<BTreeMap<String, AllocationMap>> {
    let cfg = PackageConfig::new(app_toml, verbose, edges)?;

    // Verify that our dump configuration is correct (or absent)
    check_dump_config(&cfg.toml)?;

    // If we're using filters, we change behavior at the end. Record this in a
    // convenient flag, running other checks as well.
    let (partial_build, tasks_to_build): (bool, BTreeSet<&str>) =
        if let Some(task_names) = tasks_to_build.as_ref() {
            check_task_names(&cfg.toml, task_names)?;
            (true, task_names.iter().map(|p| p.as_str()).collect())
        } else {
            assert!(!cfg.toml.tasks.contains_key("kernel"));
            check_task_priorities(&cfg.toml)?;
            (
                false,
                cfg.toml
                    .tasks
                    .keys()
                    .map(|p| p.as_str())
                    .chain(std::iter::once("kernel"))
                    .collect(),
            )
        };

    std::fs::create_dir_all(&cfg.dist_dir)?;
    if dirty_ok {
        println!("note: not doing a clean build because you asked for it");
    } else {
        check_rebuild(&cfg.toml)?;
    }

    // Build all tasks (which are relocatable executables, so they are not
    // statically linked yet). For now, we build them one by one and ignore the
    // return value, because we're going to link them regardless of whether the
    // build changed.
    for name in cfg.toml.tasks.keys() {
        if tasks_to_build.contains(name.as_str()) {
            build_task(&cfg, name)?;
        }
    }

    // Calculate the sizes of tasks, assigning dummy sizes to tasks that
    // aren't active in this build.
    let task_sizes: HashMap<_, _> = cfg
        .toml
        .tasks
        .keys()
        .map(|name| {
            let size = if tasks_to_build.contains(name.as_str()) {
                link_dummy_task(&cfg, name, &cfg.toml.image_names[0])?;
                task_size(&cfg, name)
            } else {
                // Dummy allocations
                let out: IndexMap<_, _> =
                    [("flash", 64), ("ram", 64)].into_iter().collect();
                Ok(out)
            };
            size.map(|sz| (name.as_str(), sz))
        })
        .collect::<Result<_, _>>()?;

    // Allocate memories.
    let allocated =
        allocate_all(&cfg.toml, &task_sizes, cfg.toml.caboose.as_ref())?;

    for image_name in &cfg.toml.image_names {
        // Build each task.
        let mut all_output_sections = BTreeMap::default();

        std::fs::create_dir_all(&cfg.img_dir(image_name))?;
        let (allocs, memories) = allocated
            .get(image_name)
            .ok_or_else(|| anyhow!("failed to get image name"))?;

        // Check external regions, which cannot be used for normal allocations
        let alloc_regions = allocs.regions();
        for (task_name, task) in cfg.toml.tasks.iter() {
            for r in &task.extern_regions {
                if let Some(v) = alloc_regions.get(r) {
                    bail!(
                        "cannot use region '{r}' as extern region in \
                        '{task_name}' because it's used as a normal region by \
                        [{}]",
                        v.join(", ")
                    );
                }
            }
        }

        let mut extern_regions = MultiMap::new();
        for (task_name, task) in cfg.toml.tasks.iter() {
            for r in &task.extern_regions {
                extern_regions.insert(r, task_name.clone());
            }
        }

        // Build all relevant tasks, collecting entry points into a HashMap.  If
        // we're doing a partial build, then assign a dummy entry point into
        // the HashMap, because the kernel kconfig will still need it.
        let mut entry_points: HashMap<_, _> = cfg
            .toml
            .tasks
            .keys()
            .map(|name| {
                let ep = if tasks_to_build.contains(name.as_str()) {
                    // Link tasks regardless of whether they have changed, because
                    // we don't want to track changes in the other linker input
                    // (task-link.x, memory.x, table.ld, etc)
                    link_task(&cfg, name, image_name, allocs)?;
                    task_entry_point(
                        &cfg,
                        name,
                        image_name,
                        &mut all_output_sections,
                    )
                } else {
                    // Dummy entry point
                    Ok(allocs.tasks[name]["flash"].start)
                };
                ep.map(|ep| (name.clone(), ep))
            })
            .collect::<Result<_, _>>()?;

        // Add an empty output section for the caboose
        //
        // This has to be done before building the kernel, because the caboose
        // is included in the total image size that's patched into the kernel
        // header.
        if let Some(caboose) = &cfg.toml.caboose {
            if (caboose.size as usize) < std::mem::size_of::<u32>() * 2 {
                bail!("caboose is too small; must fit at least 2x u32");
            }

            for t in &caboose.tasks {
                if !cfg.toml.tasks.contains_key(t) {
                    bail!("caboose specifies invalid task {t}");
                }
            }

            let (_, caboose_range) = allocs.caboose.as_ref().unwrap();
            // The caboose has the format
            // [CABOOSE_MAGIC, ..., MAX_LENGTH]
            // where all words in between are initialized to u32::MAX
            //
            // The final word in the caboose is the caboose length, so that we
            // can decode the caboose start by looking at it while only knowing
            // total image size.  The first word is CABOOSE_MAGIC, so we can
            // check that a valid caboose exists.  Everything else is left to
            // the user.
            let mut caboose_data = vec![0xFF; caboose.size as usize];
            caboose_data[caboose.size as usize - 4..]
                .copy_from_slice(&caboose.size.to_le_bytes());
            caboose_data[0..4]
                .copy_from_slice(&abi::CABOOSE_MAGIC.to_le_bytes());

            all_output_sections.insert(
                caboose_range.start,
                LoadSegment {
                    source_file: "caboose".into(),
                    data: caboose_data,
                },
            );
            entry_points.insert("caboose".to_string(), caboose_range.start);
        }

        // Build the kernel!
        let kern_build = if tasks_to_build.contains("kernel") {
            Some(build_kernel(
                &cfg,
                allocs,
                &mut all_output_sections,
                &cfg.toml.memories(image_name)?,
                &entry_points,
                image_name,
            )?)
        } else {
            None
        };

        // If we've done a partial build (which may have included the kernel),
        // bail out here before linking stuff.
        if partial_build {
            return Ok(allocated);
        }

        // Print stats on memory usage
        let starting_memories = cfg.toml.memories(image_name)?;
        for (name, range) in &starting_memories {
            println!(
                "{:<7} = {:#010x}..{:#010x}",
                name, range.start, range.end
            );
        }
        println!("Used:");
        for (name, new_range) in memories {
            print!("  {:<8} ", format!("{name}:"));

            if let Some(tasks) = extern_regions.get_vec(name) {
                println!("extern region ({})", tasks.join(", "));
            } else {
                let orig_range = &starting_memories[name];
                let size = new_range.start - orig_range.start;
                let percent = size * 100 / (orig_range.end - orig_range.start);

                println!("{size:#x} ({percent}%)");
            }
        }

        // Generate a RawHubrisImage, which is our source of truth for combined
        // images and is used to generate all outputs.
        let (kentry, _ksymbol_table) = kern_build.unwrap();

        let flash = cfg
            .toml
            .memories(image_name)?
            .get(&"flash".to_string())
            .ok_or_else(|| anyhow!("failed to get flash region"))?
            .clone();
        let raw_output_sections: BTreeMap<u32, Vec<u8>> = all_output_sections
            .into_iter()
            .map(|(k, v)| (k, v.data))
            .filter(|(k, _v)| flash.contains(k))
            .collect();
        let raw_image = hubtools::RawHubrisImage::from_segments(
            &raw_output_sections,
            kentry,
            0xFF,
        )
        .context("constructing image from segments with hubtools")?;

        write_gdb_script(&cfg, image_name)?;
        let archive_name = build_archive(&cfg, image_name, raw_image)?;

        // Post-build modifications: populate a default caboose if requested
        if let Some(caboose) = &cfg.toml.caboose {
            if caboose.default {
                let mut archive =
                    hubtools::RawHubrisArchive::load(&archive_name)
                        .context("loading archive with hubtools")?;
                // The Git hash is included in the default caboose under the key
                // `GITC`, so we don't include it in the pseudo-version.
                archive
                    .write_default_caboose(Some(&"0.0.0-git".to_owned()))
                    .context("writing caboose into archive")?;
                archive.overwrite().context("overwriting archive")?;
            }
        }

        // Post-build modifications: sign the image if requested
        if let Some(signing) = &cfg.toml.signing {
            let mut archive = hubtools::RawHubrisArchive::load(&archive_name)
                .context("loading archive with hubtools")?;
            let private_key = std::fs::read_to_string(
                cfg.app_src_dir.join(&signing.certs.private_key),
            )
            .with_context(|| {
                format!(
                    "could not read private key {:?}",
                    signing.certs.private_key
                )
            })?;
            let root_certs = signing
                .certs
                .root_certs
                .iter()
                .map(|c| {
                    std::fs::read(cfg.app_src_dir.join(c))
                        .context(format!("could not read root cert {c:?}",))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let signing_certs = signing
                .certs
                .signing_certs
                .iter()
                .map(|c| {
                    std::fs::read(cfg.app_src_dir.join(c))
                        .context(format!("could not read signing cert {c:?}",))
                })
                .collect::<Result<Vec<_>, _>>()?;

            archive.sign(
                signing_certs,
                root_certs.clone(),
                &private_key,
                0, // execution address (TODO)
            )?;

            let boot_pin = lpc55_areas::BootErrorPin::new(
                signing.boot_error_gpio.port,
                signing.boot_error_gpio.pin,
            )
            .ok_or_else(|| {
                anyhow!("invalid boot error pin {:?}", signing.boot_error_gpio)
            })?;
            archive.set_cmpa(
                signing.dice.clone(),
                signing.enable_secure_boot,
                signing.cmpa_settings,
                signing.default_isp,
                lpc55_areas::BootSpeed::Fro96mhz,
                boot_pin,
                root_certs,
            )?;
            archive.set_cfpa(
                signing.cfpa_settings,
                [signing.rotk0, signing.rotk1, signing.rotk2, signing.rotk3],
            )?;
            archive.overwrite()?;
        }

        // Unzip the signed + caboose'd images into our build directory
        let archive = hubtools::RawHubrisArchive::load(&archive_name)
            .context("loading archive with hubtools")?;
        for ext in ["elf", "bin"] {
            let name = format!("final.{}", ext);
            let file_data = archive
                .extract_file(&format!("img/{name}"))
                .context("extracting signed file from archive")?;
            std::fs::write(cfg.img_file(&name, image_name), file_data)?;
        }
    }
    Ok(allocated)
}

fn write_gdb_script(cfg: &PackageConfig, image_name: &str) -> Result<()> {
    // Humility doesn't know about images right now. The gdb symbol file
    // paths all assume a flat layout with everything in dist. For now,
    // match what humility expects. If a build file ever contains multiple
    // images this will need to be fixed!
    let mut gdb_script = File::create(cfg.img_file("script.gdb", image_name))?;
    writeln!(
        gdb_script,
        "add-symbol-file {}",
        cfg.dist_file("kernel").to_slash().unwrap()
    )?;
    for name in cfg.toml.tasks.keys() {
        writeln!(
            gdb_script,
            "add-symbol-file {}",
            cfg.dist_file(name).to_slash().unwrap()
        )?;
    }
    for (path, remap) in &cfg.remap_paths {
        let mut path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("Could not convert path{:?} to str", path))?
            .to_string();

        // Even on Windows, GDB expects path components to be separated by '/',
        // so we tweak the path here so that remapping works.
        if cfg!(windows) {
            path_str = path_str.replace('\\', "/");
        }
        writeln!(gdb_script, "set substitute-path {} {}", remap, path_str)?;
    }
    Ok(())
}

fn build_archive(
    cfg: &PackageConfig,
    image_name: &str,
    raw_image: hubtools::RawHubrisImage,
) -> Result<PathBuf> {
    // Bundle everything up into an archive.
    let archive_path =
        cfg.img_file(format!("build-{}.zip", cfg.toml.name), image_name);
    let mut archive = Archive::new(&archive_path)?;

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
        - img/ contains the final firmware images.\n\
        - debug/ contains OpenOCD and GDB scripts, if available.\n",
    )?;

    let (git_rev, git_dirty) = get_git_status()?;
    archive.text(
        "git-rev",
        format!("{}{}", git_rev, if git_dirty { "-dirty" } else { "" }),
    )?;
    archive.copy(&cfg.app_toml_file, "app.toml")?;
    if let Some(patches) = cfg.patches.as_ref() {
        archive
            .text(
                "patches.toml",
                toml::to_string(patches)
                    .context("Could not serialize patches")?,
            )
            .context("Could not write patches.toml")?;
    }
    let chip_dir = cfg.app_src_dir.join(cfg.toml.chip.clone());
    let chip_file = chip_dir.join("chip.toml");
    let chip_filename = chip_file.file_name().unwrap();
    archive.copy(&chip_file, &chip_filename)?;

    let elf_dir = PathBuf::from("elf");
    let tasks_dir = elf_dir.join("task");
    for name in cfg.toml.tasks.keys() {
        archive.copy(cfg.img_file(name, image_name), tasks_dir.join(name))?;
    }
    archive.copy(cfg.img_file("kernel", image_name), elf_dir.join("kernel"))?;

    let img_dir = PathBuf::from("img");
    archive.binary(img_dir.join("final.elf"), raw_image.to_elf()?)?;
    archive.binary(img_dir.join("final.bin"), raw_image.to_binary()?)?;

    //
    // To allow for the image to be flashed based only on the archive (e.g.,
    // by Humility), we pull in our flash configuration, flatten it to pull in
    // any external configuration files, serialize it, and add it to the
    // archive.
    //
    if let Some(mut config) =
        crate::flash::config(cfg.toml.board.as_str(), &chip_dir)?
    {
        config.flatten()?;
        archive.text(
            img_dir.join("flash.ron"),
            ron::ser::to_string_pretty(
                &config,
                ron::ser::PrettyConfig::default(),
            )?,
        )?;
    }

    let debug_dir = PathBuf::from("debug");
    archive.copy(
        cfg.img_file("script.gdb", image_name),
        debug_dir.join("script.gdb"),
    )?;

    if let Some(auxflash) = cfg.toml.auxflash.as_ref() {
        let file = cfg.dist_file("auxi.tlvc");
        std::fs::write(&file, &auxflash.data)
            .context(format!("Failed to write auxi to {:?}", file))?;
        archive.copy(cfg.dist_file("auxi.tlvc"), img_dir.join("auxi.tlvc"))?;
    }

    // Copy `openocd.cfg` into the archive if it exists; it's not used for
    // the LPC55 boards.
    let openocd_cfg = chip_dir.join("openocd.cfg");
    if openocd_cfg.exists() {
        archive.copy(openocd_cfg, debug_dir.join("openocd.cfg"))?;
    }
    archive
        .copy(chip_dir.join("openocd.gdb"), debug_dir.join("openocd.gdb"))?;

    let mut metadata = None;

    //
    // Iterate over tasks looking for elements that should be copied into
    // the archive.  These are specified by the "copy-to-archive" array,
    // which consists of keys into the config table; the values associated
    // with these keys have the names of the files to add to the archive.
    // All files added to the archive for a particular task will be in
    // a directory dedicated to that task; all such directories will
    // themselves be subdirectories in the "task" directory.
    //
    for (name, task) in &cfg.toml.tasks {
        for c in &task.copy_to_archive {
            match &task.config {
                None => {
                    bail!(
                        "task {name}: {c} is specified to be copied \
                        into archive, but config table is missing"
                    );
                }
                Some(config) => match config.get(c) {
                    Some(ordered_toml::Value::String(s)) => {
                        //
                        // This is a bit of a heavy hammer:  we need the
                        // directory name for the task to find the file to be
                        // copied into the archive, so we're going to iterate
                        // over all packages to find the crate assocated with
                        // this task.  (We cache the metadata itself, as it
                        // takes on the order of ~150 ms to gather.)
                        //
                        use cargo_metadata::MetadataCommand;
                        let metadata = match metadata.as_ref() {
                            Some(m) => m,
                            None => {
                                let d = MetadataCommand::new()
                                    .manifest_path("./Cargo.toml")
                                    .exec()?;
                                metadata.get_or_insert(d)
                            }
                        };

                        let pkg = metadata
                            .packages
                            .iter()
                            .find(|p| p.name == task.name)
                            .unwrap();

                        let dir = pkg.manifest_path.parent().unwrap();

                        let f = dir.join(s);
                        let task_dir = PathBuf::from("task").join(name).join(s);
                        archive.copy(f, task_dir).with_context(|| {
                            format!(
                                "task {name}: failed to copy \"{s}\" in {} \
                                into the archive",
                                dir.display()
                            )
                        })?;
                    }
                    Some(_) => {
                        bail!(
                            "task {name}: {c} is specified to be copied into \
                            the archive, but isn't a string in the config table"
                        );
                    }
                    None => {
                        bail!(
                            "task {name}: {c} is specified to be copied into \
                            the archive, but is missing in the config table"
                        );
                    }
                },
            }
        }
    }

    archive.finish()?;
    Ok(archive_path)
}

fn check_task_names(toml: &Config, task_names: &[String]) -> Result<()> {
    // Quick sanity-check if we're trying to build individual tasks which
    // aren't present in the app.toml, or ran `cargo xtask build ...` without
    // any specified tasks.
    if task_names.is_empty() {
        bail!(
            "Running `cargo xtask build` without specifying tasks has no effect.\n\
             Did you mean to run `cargo xtask dist`?"
        );
    }
    let all_tasks = toml.tasks.keys().collect::<BTreeSet<_>>();
    if let Some(name) = task_names
        .iter()
        .filter(|name| name.as_str() != "kernel")
        .find(|name| !all_tasks.contains(name))
    {
        bail!(toml.task_name_suggestion(name))
    }
    Ok(())
}

/// Checks the buildstamp file and runs `cargo clean` if invalid
fn check_rebuild(toml: &Config) -> Result<()> {
    let buildstamp_file = Path::new("target").join("buildstamp");
    let rebuild = match std::fs::read(&buildstamp_file) {
        Ok(contents) => {
            if let Ok(contents) = std::str::from_utf8(&contents) {
                if let Ok(cmp) = u64::from_str_radix(contents, 16) {
                    toml.buildhash != cmp
                } else {
                    println!("buildstamp file contents unknown; re-building.");
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
    };
    // if we need to rebuild, we should clean everything before we start building
    if rebuild {
        println!("app.toml has changed; rebuilding all tasks");
        let mut names = vec![toml.kernel.name.as_str()];
        for name in toml.tasks.keys() {
            // This may feel redundant: don't we already have the name?
            // Well, consider our supervisor:
            //
            // [tasks.jefe]
            // name = "task-jefe"
            //
            // The "name" in the key is `jefe`, but the package (crate)
            // name is in `tasks.jefe.name`, and that's what we need to
            // give to `cargo`.
            names.push(toml.tasks[name].name.as_str());
        }
        cargo_clean(&names, &toml.target)?;
    }

    // now that we're clean, update our buildstamp file; any failure to build
    // from here on need not trigger a clean
    std::fs::write(&buildstamp_file, format!("{:x}", toml.buildhash))?;

    Ok(())
}

#[derive(Debug, Hash)]
struct LoadSegment {
    source_file: PathBuf,
    data: Vec<u8>,
}

/// Builds a specific task
fn build_task(cfg: &PackageConfig, name: &str) -> Result<()> {
    // Use relocatable linker script for this build
    fs::copy("build/task-rlink.x", "target/link.x")?;
    // Append any task-specific sections.
    {
        let task_toml = &cfg.toml.tasks[name];
        let mut linkscr = std::fs::OpenOptions::new()
            .create(false)
            .append(true)
            .open("target/link.x")?;
        append_task_sections(&mut linkscr, Some(&task_toml.sections))?;
    }

    let build_config = cfg
        .toml
        .task_build_config(name, cfg.verbose, Some(&cfg.sysroot))
        .unwrap();
    build(cfg, name, build_config, true)
        .context(format!("failed to build {}", name))
}

/// Link a specific task
fn link_task(
    cfg: &PackageConfig,
    name: &str,
    image_name: &str,
    allocs: &Allocations,
) -> Result<()> {
    println!("linking task '{}'", name);
    let task_toml = &cfg.toml.tasks[name];

    let extern_regions = cfg.toml.extern_regions_for(name, image_name)?;
    generate_task_linker_script(
        "memory.x",
        &allocs.tasks[name],
        Some(&task_toml.sections),
        task_toml.stacksize.or(cfg.toml.stacksize).ok_or_else(|| {
            anyhow!("{}: no stack size specified and there is no default", name)
        })?,
        &cfg.toml.all_regions("flash".to_string())?,
        &extern_regions,
        image_name,
    )
    .context(format!("failed to generate linker script for {}", name))?;
    fs::copy("build/task-link.x", "target/link.x")?;

    // Link the static archive
    link(
        cfg,
        &format!("{}.elf", name),
        &format!("{}/{}", image_name, name),
    )
}

/// Link a specific task using a dummy linker script that gives it all possible
/// memory; this is used to determine its true size.
fn link_dummy_task(
    cfg: &PackageConfig,
    name: &str,
    image_name: &str,
) -> Result<()> {
    let task_toml = &cfg.toml.tasks[name];

    let memories = cfg
        .toml
        .memories(&cfg.toml.image_names[0])?
        .into_iter()
        .collect();
    let extern_regions = cfg.toml.extern_regions_for(name, image_name)?;

    generate_task_linker_script(
        "memory.x",
        &memories, // ALL THE SPACE
        Some(&task_toml.sections),
        task_toml.stacksize.or(cfg.toml.stacksize).ok_or_else(|| {
            anyhow!("{}: no stack size specified and there is no default", name)
        })?,
        &cfg.toml.all_regions("flash".to_string())?,
        &extern_regions,
        &cfg.toml.image_names[0],
    )
    .context(format!("failed to generate linker script for {}", name))?;
    fs::copy("build/task-tlink.x", "target/link.x")?;

    // Link the static archive
    link(cfg, &format!("{}.elf", name), &format!("{}.tmp", name))
}

fn task_size<'a, 'b>(
    cfg: &'a PackageConfig,
    name: &'b str,
) -> Result<IndexMap<&'a str, u64>> {
    let task = &cfg.toml.tasks[name];
    let stacksize = task.stacksize.or(cfg.toml.stacksize).unwrap();
    load_task_size(&cfg.toml, name, stacksize)
}

/// Loads a given task's ELF file, populating `all_output_sections` and
/// returning its entry point.
fn task_entry_point(
    cfg: &PackageConfig,
    name: &str,
    image_name: &str,
    all_output_sections: &mut BTreeMap<u32, LoadSegment>,
) -> Result<u32> {
    let task_toml = &cfg.toml.tasks[name];
    resolve_task_slots(cfg, name, image_name)?;

    let mut symbol_table = BTreeMap::default();
    let (ep, flash) = load_elf(
        &cfg.img_file(name, image_name),
        all_output_sections,
        &mut symbol_table,
    )?;

    if let Some(required) = task_toml.max_sizes.get("flash") {
        if flash > *required as usize {
            bail!(
                "{} has insufficient flash: specified {} bytes, needs {}",
                task_toml.name,
                required,
                flash
            );
        }
    }
    Ok(ep)
}

fn build_kernel(
    cfg: &PackageConfig,
    allocs: &Allocations,
    all_output_sections: &mut BTreeMap<u32, LoadSegment>,
    all_memories: &IndexMap<String, Range<u32>>,
    entry_points: &HashMap<String, u32>,
    image_name: &str,
) -> Result<(u32, BTreeMap<String, u32>)> {
    let mut image_id = fnv::FnvHasher::default();
    all_output_sections.hash(&mut image_id);

    // Format the descriptors for the kernel build.
    let kconfig =
        make_kconfig(&cfg.toml, &allocs.tasks, entry_points, image_name)?;
    let kconfig = ron::ser::to_string(&kconfig)?;

    kconfig.hash(&mut image_id);
    allocs.hash(&mut image_id);

    generate_kernel_linker_script(
        "memory.x",
        &allocs.kernel,
        cfg.toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &cfg.toml.all_regions("flash".to_string())?,
        &image_name,
    )?;

    fs::copy("build/kernel-link.x", "target/link.x")?;

    let image_id = image_id.finish();

    // Build the kernel.
    let build_config = cfg.toml.kernel_build_config(
        cfg.verbose,
        &[
            ("HUBRIS_KCONFIG", &kconfig),
            ("HUBRIS_IMAGE_ID", &format!("{}", image_id)),
        ],
        Some(&cfg.sysroot),
    );
    build(cfg, "kernel", build_config, false)?;
    if update_image_header(
        cfg,
        &cfg.dist_file("kernel"),
        &cfg.img_file("kernel.modified", image_name),
        all_memories,
        all_output_sections,
    )? {
        std::fs::copy(
            &cfg.dist_file("kernel"),
            cfg.img_file("kernel.orig", image_name),
        )?;
        std::fs::copy(
            &cfg.img_file("kernel.modified", image_name),
            cfg.img_file("kernel", image_name),
        )?;
    } else {
        std::fs::copy(
            &cfg.dist_file("kernel"),
            cfg.img_file("kernel", image_name),
        )?;
    }

    let mut ksymbol_table = BTreeMap::default();
    let (kentry, _) = load_elf(
        &cfg.img_file("kernel", image_name),
        all_output_sections,
        &mut ksymbol_table,
    )?;
    Ok((kentry, ksymbol_table))
}

/// Adjusts the hubris image header in the ELF file.
/// Returns true if the header was found and updated,
/// false otherwise.
fn update_image_header(
    cfg: &PackageConfig,
    input: &Path,
    output: &Path,
    map: &IndexMap<String, Range<u32>>,
    all_output_sections: &mut BTreeMap<u32, LoadSegment>,
) -> Result<bool> {
    use goblin::container::Container;

    let mut file_image = std::fs::read(input)?;
    let elf = goblin::elf::Elf::parse(&file_image)?;

    if elf.header.container()? != Container::Little {
        bail!("where did you get a big-endian image?");
    }
    if elf.header.e_machine != goblin::elf::header::EM_ARM {
        bail!("this is not an ARM file");
    }

    // Good enough.
    for sec in &elf.section_headers {
        if let Some(name) = elf.shdr_strtab.get_at(sec.sh_name) {
            if name == ".header"
                && (sec.sh_size as usize)
                    >= core::mem::size_of::<abi::ImageHeader>()
            {
                let flash = map.get("flash").unwrap();

                // Compute the total image size by finding the highest address
                // from all the tasks built.
                let end = all_output_sections
                    .iter()
                    .filter(|(addr, _sec)| flash.contains(addr))
                    .map(|(&addr, sec)| addr + sec.data.len() as u32)
                    .max();
                // Normally, at this point, all tasks are built, so we can
                // compute the actual number. However, in the specific case of
                // `xtask build kernel`, we need a result from this calculation
                // but `end` will be `None`. Substitute a placeholder:
                let end = end.unwrap_or(flash.start);

                let len = end - flash.start;

                let header = abi::ImageHeader {
                    version: cfg.toml.version,
                    epoch: cfg.toml.epoch,
                    magic: abi::HEADER_MAGIC,
                    total_image_len: len as u32,
                    ..Default::default()
                };

                header
                    .write_to_prefix(
                        &mut file_image[(sec.sh_offset as usize)..],
                    )
                    .unwrap();
                std::fs::write(output, &file_image)?;
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Checks our dump config:  that if we have a dump agent, it has a task slot
/// for Jefe (denoting task dump support); that every memory that the dump
/// agent is using it also being used by Jefe; that if dumps are enabled, the
/// support has been properly enabled in the kernel.  (Conversely, we assure
/// that if dump support is enabled, the other components are properly
/// configured.)  Yes, this is some specific knowledge of the system to encode
/// here, but we want to turn a preventable, high-consequence run-time error
/// (namely, Jefe attempting accessing memory that it doesn't have access to
/// or making a system call that is unsupported) into a compile-time one.
fn check_dump_config(toml: &Config) -> Result<()> {
    let dump_support = toml.kernel.features.iter().find(|&f| f == "dump");

    if let Some(task) = toml.tasks.get("dump_agent") {
        if task.extern_regions.is_empty() {
            bail!(
                "dump agent misconfiguration: dump agent is present \
                but does not have any external regions for dumping"
            );
        }

        if task.task_slots.get("jefe").is_none() {
            bail!(
                "dump agent misconfiguration: dump agent is present \
                but has not been configured to depend on jefe"
            );
        }

        //
        // We have a dump agent, and it has a slot for Jefe, denoting that
        // it is configured for task dumps; now we want to check that Jefe
        // (1) has the dump feature enabled (2) has extern regions and
        // (3) uses everything that the dump agent is using.
        //
        let jefe = toml.tasks.get("jefe").context("missing jefe?")?;

        if !jefe.features.iter().any(|f| f == "dump") {
            bail!(
                "dump agent/jefe misconfiguration: dump agent depends \
                on jefe, but jefe does not have the dump feature enabled"
            );
        }

        if dump_support.is_none() {
            bail!(
                "dump agent is present and system is otherwise configured \
                for dumping, but kernel does not have the dump feature enabled
            "
            );
        }

        for u in &task.extern_regions {
            if !jefe.extern_regions.iter().any(|j| j == u) {
                bail!(
                    "dump agent/jefe misconfiguration: dump agent has \
                    {u} as an extern-region and depends on jefe, but jefe \
                    does not have {u} as an extern-region"
                );
            }
        }
    } else if dump_support.is_some() {
        bail!("kernel dump support is enabled, but dump agent is missing");
    }

    Ok(())
}

/// Prints warning messages about priority inversions
fn check_task_priorities(toml: &Config) -> Result<()> {
    let idle_priority = toml.tasks["idle"].priority;
    for (i, (name, task)) in toml.tasks.iter().enumerate() {
        for callee in task.task_slots.values() {
            let p = toml
                .tasks
                .get(callee)
                .ok_or_else(|| anyhow!("Invalid task-slot: {}", callee))?
                .priority;
            if p >= task.priority && name != callee {
                // TODO: once all priority inversions are fixed, return an
                // error so no more can be introduced
                eprint!("{}", "Priority inversion: ".red());
                eprintln!(
                    "task {} (priority {}) calls into {} (priority {})",
                    name, task.priority, callee, p
                );
            }
        }
        if task.priority >= idle_priority && name != "idle" {
            bail!("task {} has priority that's >= idle priority", name);
        } else if i == 0 && task.priority != 0 {
            bail!("Supervisor task ({}) is not at priority 0", name);
        } else if i != 0 && task.priority == 0 {
            bail!("Task {} is not the supervisor, but has priority 0", name,);
        }
    }

    Ok(())
}

fn generate_task_linker_script(
    name: &str,
    map: &BTreeMap<String, Range<u32>>,
    sections: Option<&IndexMap<String, String>>,
    stacksize: u32,
    images: &IndexMap<String, Range<u32>>,
    extern_regions: &IndexMap<String, Range<u32>>,
    image_name: &str,
) -> Result<()> {
    // Put the linker script somewhere the linker can find it
    let mut linkscr = File::create(Path::new(&format!("target/{}", name)))?;

    fn emit(linkscr: &mut File, sec: &str, o: u32, l: u32) -> Result<()> {
        writeln!(
            linkscr,
            "{} (rwx) : ORIGIN = {:#010x}, LENGTH = {:#010x}",
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
    append_image_names(&mut linkscr, images, image_name)?;
    append_extern_regions(&mut linkscr, extern_regions)?;
    append_task_sections(&mut linkscr, sections)?;

    Ok(())
}

fn append_image_names(
    linkscr: &mut std::fs::File,

    images: &IndexMap<String, Range<u32>>,
    image_name: &str,
) -> Result<()> {
    for (name, out) in images {
        if name == image_name {
            writeln!(linkscr, "__this_image = {:#010x};", out.start)?;
        }
        writeln!(
            linkscr,
            "__IMAGE_{}_BASE = {:#010x};",
            name.to_ascii_uppercase(),
            out.start
        )?;
        writeln!(
            linkscr,
            "__IMAGE_{}_END = {:#010x};",
            name.to_ascii_uppercase(),
            out.end
        )?;
    }

    Ok(())
}

fn append_extern_regions(
    linkscr: &mut std::fs::File,
    extern_regions: &IndexMap<String, Range<u32>>,
) -> Result<()> {
    for (name, out) in extern_regions {
        writeln!(
            linkscr,
            "__REGION_{}_BASE = {:#010x};",
            name.to_ascii_uppercase(),
            out.start
        )?;
        writeln!(
            linkscr,
            "__REGION_{}_END = {:#010x};",
            name.to_ascii_uppercase(),
            out.end
        )?;
    }

    Ok(())
}

fn append_task_sections(
    out: &mut std::fs::File,
    sections: Option<&IndexMap<String, String>>,
) -> Result<()> {
    // The task may have defined additional section-to-memory mappings.
    if let Some(map) = sections {
        writeln!(out, "SECTIONS {{")?;
        for (section, memory) in map {
            writeln!(out, "  .{} (NOLOAD) : ALIGN(4) {{", section)?;
            writeln!(out, "    *(.{} .{}.*);", section, section)?;
            writeln!(out, "  }} > {}", memory.to_ascii_uppercase())?;
        }
        writeln!(out, "}} INSERT AFTER .uninit")?;
    }

    Ok(())
}

fn generate_kernel_linker_script(
    name: &str,
    map: &BTreeMap<String, Range<u32>>,
    stacksize: u32,
    images: &IndexMap<String, Range<u32>>,
    image_name: &str,
) -> Result<()> {
    // Put the linker script somewhere the linker can find it
    let mut linkscr =
        File::create(Path::new(&format!("target/{}", name))).unwrap();

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
                "STACK (rw) : ORIGIN = {:#010x}, LENGTH = {:#010x}",
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
            "{} (rwx) : ORIGIN = {:#010x}, LENGTH = {:#010x}",
            name,
            start,
            end - start
        )
        .unwrap();
    }
    writeln!(linkscr, "}}").unwrap();
    writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
    writeln!(linkscr, "_stack_base = {:#010x};", stack_base.unwrap()).unwrap();
    writeln!(linkscr, "_stack_start = {:#010x};", stack_start.unwrap())
        .unwrap();
    writeln!(
        linkscr,
        "_HUBRIS_IMAGE_HEADER_ALIGN = {:#x};",
        std::mem::align_of::<abi::ImageHeader>()
    )
    .unwrap();
    writeln!(
        linkscr,
        "_HUBRIS_IMAGE_HEADER_SIZE = {:#x};",
        std::mem::size_of::<abi::ImageHeader>()
    )
    .unwrap();

    append_image_names(&mut linkscr, images, image_name)?;
    Ok(())
}

fn build(
    cfg: &PackageConfig,
    name: &str,
    build_config: BuildConfig,
    reloc: bool,
) -> Result<()> {
    println!("building crate {}", build_config.crate_name);

    let mut cmd = build_config.cmd("rustc");
    cmd.arg("--release");

    // We're capturing stderr (for diagnosis), so `cargo` won't automatically
    // turn on color.  If *we* are a TTY, then force it on.
    if atty::is(Stream::Stderr) {
        cmd.arg("--color");
        cmd.arg("always");
    }

    // This works because we control the environment in which we're about
    // to invoke cargo, and never modify CARGO_TARGET in that environment.
    let cargo_out = Path::new("target").to_path_buf();

    let remap_path_prefix: String = cfg
        .remap_paths
        .iter()
        .map(|r| format!(" --remap-path-prefix={}={}", r.0.display(), r.1))
        .collect();
    cmd.env(
        "RUSTFLAGS",
        &format!(
            "-C link-arg=-z -C link-arg=common-page-size=0x20 \
             -C link-arg=-z -C link-arg=max-page-size=0x20 \
             -C llvm-args=--enable-machine-outliner=never \
             -C overflow-checks=y \
             -C metadata={} \
             {}
             ",
            cfg.link_script_hash, remap_path_prefix,
        ),
    );
    cmd.arg("--");

    // We use attributes to conditionally import based on feature flags;
    // invalid combinations of features often create duplicate attributes,
    // which causes the later one to go unused.  Let's detect this explicitly!
    cmd.arg("-Dunused_attributes");

    cmd.arg("-C")
        .arg("link-arg=-Tlink.x")
        .arg("-L")
        .arg(format!("{}", cargo_out.display()));
    if reloc {
        cmd.arg("-C").arg("link-arg=-r");
    }

    if cfg.edges {
        let mut tree = build_config.cmd("tree");
        tree.arg("--edges").arg("features").arg("--verbose");
        println!(
            "Crate: {}\nRunning cargo {:?}",
            build_config.crate_name, tree
        );
        let tree_status = tree
            .status()
            .context(format!("failed to run edge ({:?})", tree))?;
        if !tree_status.success() {
            bail!("tree command failed, see output for details");
        }
    }

    // File generated by the build system
    let src_file = cargo_out.join(build_config.out_path);

    // We want to (a) store stderr to a buffer to check for a particular error
    // message, and (b) print data from stderr to the terminal as quickly as we
    // can.  To enable these dueling priorities, we spawn a task with stderr
    // going to a pipe, then use a separate thread to constantly read from that
    // pipe.
    let mut child = cmd
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start child process")?;

    let mut child_stderr =
        child.stderr.take().context("Failed to take stderr")?;
    let reader_thread = std::thread::spawn(move || {
        let mut out_bytes = vec![];
        let mut stderr = std::io::stderr();
        loop {
            let mut buf = [0u8; 256];
            let num = child_stderr.read(&mut buf).unwrap();
            if num == 0 {
                break;
            }

            // Immediately echo `stderr` back out, using a raw write because it
            // may contain terminal control characters
            stderr.write_all(&buf[0..num]).unwrap();
            stderr.flush().unwrap();

            out_bytes.extend(buf[0..num].into_iter());
        }
        out_bytes
    });

    let status = child
        .wait()
        .context(format!("failed to run rustc ({:?})", cmd))?;
    let stderr_bytes = reader_thread.join().unwrap();

    if !status.success() {
        // We've got a special case here: if the kernel memory is too small,
        // then the build will fail with a cryptic linker error.  We can't
        // convert `status.stderr` to a `String`, because it probably contains
        // terminal control characters, so do a raw `&[u8]` search instead.
        if name == "kernel"
            && memchr::memmem::find(
                &stderr_bytes,
                b"will not fit in region".as_slice(),
            )
            .is_some()
        {
            bail!(
                "command failed, see output for details\n    \
                         The kernel may have run out of space; try increasing \
                         its allocation in the app's TOML file"
            )
        }

        // A second special case: warn about missing notifications by suggesting
        // that they be added to the app.toml
        let re = regex::bytes::Regex::new(
            "cannot find value `([A-Z_]+)` in (crate|module) `notifications(.*)`",
        )
        .unwrap();
        let mut missing_notifications: BTreeMap<String, BTreeSet<String>> =
            BTreeMap::new();
        for c in re.captures_iter(&stderr_bytes) {
            let notification =
                std::str::from_utf8(&c.get(1).unwrap().as_bytes()).unwrap();
            let task =
                std::str::from_utf8(&c.get(3).unwrap().as_bytes()).unwrap();
            let task = if let Some(task) = task.strip_prefix("::") {
                task
            } else {
                name
            };
            missing_notifications
                .entry(task.to_owned())
                .or_default()
                .insert(notification.to_owned());
        }
        if !missing_notifications.is_empty() {
            let mut out = String::new();
            for (task, ns) in missing_notifications {
                let mut names = ns
                    .iter()
                    .map(|n| {
                        n.trim_end_matches("_MASK")
                            .trim_end_matches("_BIT")
                            .to_lowercase()
                            .replace("_", "-")
                    })
                    .collect::<Vec<_>>();
                names.sort();
                names.dedup();
                write!(&mut out, "\n- {task} is missing {names:?}")?;
            }
            bail!(
                "Missing notifications; do you need to add them to your TOML file?{out}"
            );
        }

        bail!("command failed, see output for details");
    }

    // Destination where it should be copied (using the task name rather than
    // the crate name)
    let dest = cfg.dist_file(if reloc {
        format!("{}.elf", name)
    } else {
        name.to_string()
    });

    println!("{} -> {}", src_file.display(), dest.display());
    std::fs::copy(&src_file, dest)?;

    Ok(())
}

fn link(
    cfg: &PackageConfig,
    src_file: impl AsRef<Path> + AsRef<std::ffi::OsStr>,
    dst_file: impl AsRef<Path> + AsRef<std::ffi::OsStr>,
) -> Result<()> {
    let mut ld = cfg.sysroot.clone();
    ld.extend([
        "lib",
        "rustlib",
        &cfg.host_triple,
        "bin",
        "gcc-ld",
        "ld.lld",
    ]);

    let mut cmd = Command::new(ld);
    if cfg.verbose {
        cmd.arg("--verbose");
    }

    // We expect the caller to set up our linker scripts, but copy them into
    // our working directory here
    let working_dir = &cfg.dist_dir;
    for f in ["link.x", "memory.x"] {
        std::fs::copy(format!("target/{}", f), working_dir.join(f))
            .context(format!("Could not copy {} to link dir", f))?;
    }
    assert!(AsRef::<Path>::as_ref(&src_file).is_relative());
    assert!(AsRef::<Path>::as_ref(&dst_file).is_relative());

    let m = match cfg.toml.target.as_str() {
        "thumbv6m-none-eabi"
        | "thumbv7em-none-eabihf"
        | "thumbv8m.main-none-eabihf" => "armelf",
        _ => bail!("No target emulation for '{}'", cfg.toml.target),
    };
    cmd.arg(src_file);
    cmd.arg("-o").arg(dst_file);
    cmd.arg("-Tlink.x");
    cmd.arg("--gc-sections");
    cmd.arg("-m").arg(m);
    cmd.arg("-z").arg("common-page-size=0x20");
    cmd.arg("-z").arg("max-page-size=0x20");

    cmd.current_dir(working_dir);

    let status = cmd
        .status()
        .context(format!("failed to run linker ({:?})", cmd))?;

    if !status.success() {
        bail!("command failed, see output for details");
    }

    Ok(())
}

#[derive(Debug, Clone, Default, Hash)]
pub struct Allocations {
    /// Map from memory-name to address-range
    pub kernel: BTreeMap<String, Range<u32>>,
    /// Map from task-name to memory-name to address-range
    pub tasks: BTreeMap<String, BTreeMap<String, Range<u32>>>,
    /// Optional trailing caboose, located in the given region
    pub caboose: Option<(String, Range<u32>)>,
}

impl Allocations {
    /// Returns the names of every region used in allocations
    fn regions(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (region, name) in self
            .kernel
            .keys()
            .map(|k| (k, "kernel".to_owned()))
            .chain(
                self.tasks
                    .iter()
                    .flat_map(|(t, v)| v.keys().map(|k| (k, t.to_owned()))),
            )
            .chain(self.caboose.iter().map(|v| (&v.0, "caboose".to_owned())))
        {
            out.entry(region.to_owned()).or_default().push(name)
        }
        out
    }
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
pub fn allocate_all(
    toml: &Config,
    task_sizes: &HashMap<&str, IndexMap<&str, u64>>,
    caboose: Option<&CabooseConfig>,
) -> Result<BTreeMap<String, AllocationMap>> {
    // Collect all allocation requests into queues, one per memory type, indexed
    // by allocation size. This is equivalent to required alignment because of
    // the naturally-aligned-power-of-two requirement.
    //
    // We keep kernel and task requests separate so we can always service the
    // kernel first.
    //
    // The task map is: memory name -> allocation size -> queue of task name.
    // The kernel map is: memory name -> allocation size
    let kernel = &toml.kernel;
    let tasks = &toml.tasks;
    let mut result: BTreeMap<
        String,
        (Allocations, IndexMap<String, Range<u32>>),
    > = BTreeMap::new();

    for image_name in &toml.image_names {
        let mut allocs = Allocations::default();
        let mut free = toml.memories(image_name)?;
        let kernel_requests = &kernel.requires;

        let mut task_requests: BTreeMap<&str, BTreeMap<u32, VecDeque<&str>>> =
            BTreeMap::new();

        for name in tasks.keys() {
            for (mem, amt) in task_sizes[name.as_str()].iter() {
                let bytes = toml.suggest_memory_region_size(name, *amt);
                if let Some(r) = tasks[name].max_sizes.get(&mem.to_string()) {
                    if bytes > *r as u64 {
                        bail!(
                        "task {}: needs {} bytes of {} but max-sizes limits it to {}",
                        name, bytes, mem, r);
                    }
                }
                task_requests
                    .entry(mem)
                    .or_default()
                    .entry(bytes.try_into().unwrap())
                    .or_default()
                    .push_back(name.as_str());
            }
        }

        // Okay! Do memory types one by one, fitting kernel first.
        for (region, avail) in &mut free {
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
                // - Task requests larger than this alignment, in ascending
                //   order of size.

                if let Some(&sz) = k_req.take() {
                    // The kernel wants in on this.
                    allocs.kernel.insert(
                        region.to_string(),
                        allocate_k(region, sz, avail)?,
                    );
                    continue 'fitloop;
                }

                if let Some(t_reqs) = t_reqs.as_mut() {
                    for (&sz, q) in t_reqs.range_mut(..=align).rev() {
                        if let Some(task) = q.pop_front() {
                            // We can pack an equal or smaller one in.
                            let align = toml.task_memory_alignment(sz);
                            allocs
                                .tasks
                                .entry(task.to_string())
                                .or_default()
                                .insert(
                                    region.to_string(),
                                    allocate_one(region, sz, align, avail)?,
                                );
                            continue 'fitloop;
                        }
                    }

                    for (&sz, q) in t_reqs.range_mut(align + 1..) {
                        if let Some(task) = q.pop_front() {
                            // We've gotta use a larger one.
                            let align = toml.task_memory_alignment(sz);
                            allocs
                                .tasks
                                .entry(task.to_string())
                                .or_default()
                                .insert(
                                    region.to_string(),
                                    allocate_one(region, sz, align, avail)?,
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

        if let Some(caboose) = caboose {
            if toml.tasks.contains_key("caboose") {
                bail!("cannot have both a caboose and a task named 'caboose'");
            }
            if !caboose.size.is_power_of_two() {
                bail!("caboose size must be a power of two");
            }
            let avail = free.get_mut(&caboose.region).ok_or_else(|| {
                anyhow!("could not find caboose region {}", caboose.region)
            })?;
            let align = toml.task_memory_alignment(caboose.size);
            allocs.caboose = Some((
                caboose.region.clone(),
                allocate_one(&caboose.region, caboose.size, align, avail)?,
            ));
        }

        result.insert(image_name.to_string(), (allocs, free));
    }
    Ok(result)
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
    align: u32,
    avail: &mut Range<u32>,
) -> Result<Range<u32>> {
    assert!(align.is_power_of_two());

    let size_mask = align - 1;

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

/// Generate the configuration data that's passed into the kernel's build
/// system.
pub fn make_kconfig(
    toml: &Config,
    task_allocations: &BTreeMap<String, BTreeMap<String, Range<u32>>>,
    entry_points: &HashMap<String, u32>,
    image_name: &str,
) -> Result<build_kconfig::KernelConfig> {
    let mut tasks = vec![];
    let mut irqs = BTreeMap::new();

    let p2_required = toml.mpu_power_of_two_required();

    let mut flat_shared = BTreeMap::new();
    for (name, p) in &toml.peripherals {
        if p2_required && !p.size.is_power_of_two() {
            bail!(
                "memory region for peripheral '{}' is required to be \
                 a power of two, but has size {}",
                name,
                p.size
            );
        }

        flat_shared.insert(
            name.to_string(),
            build_kconfig::RegionConfig {
                base: p.address,
                size: p.size,
                attributes: build_kconfig::RegionAttributes {
                    read: true,
                    write: true,
                    execute: false,
                    special_role: Some(build_kconfig::SpecialRole::Device),
                },
            },
        );
    }
    for (name, p) in &toml.extratext {
        if p2_required && !p.size.is_power_of_two() {
            bail!(
                "memory region for extratext '{}' is required to be \
                 a power of two, but has size {}",
                name,
                p.size
            );
        }
        flat_shared.insert(
            name.to_string(),
            build_kconfig::RegionConfig {
                base: p.address,
                size: p.size,
                attributes: build_kconfig::RegionAttributes {
                    read: true,
                    write: false,
                    execute: true,
                    special_role: None,
                },
            },
        );
    }

    if let Some(c) = &toml.caboose {
        flat_shared.insert(
            "caboose".to_string(),
            build_kconfig::RegionConfig {
                base: entry_points["caboose"],
                size: c.size,
                attributes: build_kconfig::RegionAttributes {
                    read: true,
                    write: false,
                    execute: false,
                    special_role: None,
                },
            },
        );
    }

    let mut used_shared_regions = BTreeSet::new();

    for (i, (name, task)) in toml.tasks.iter().enumerate() {
        let stacksize = task.stacksize.or(toml.stacksize).unwrap();

        let flash = &task_allocations[name]["flash"];
        let entry_offset = if flash.contains(&entry_points[name]) {
            entry_points[name] - flash.start
        } else {
            bail!(
                "entry point {:#x} is not in flash range {:#x?}",
                entry_points[name],
                flash
            );
        };

        // Mark off the regions this task uses.
        for region in &task.uses {
            used_shared_regions.insert(region.as_str());
        }

        // Prep this task's shared region name set.
        let mut shared_regions: std::collections::BTreeSet<String> =
            task.uses.iter().cloned().collect();

        // Allow specified tasks to use the caboose
        if let Some(caboose) = &toml.caboose {
            if caboose.tasks.contains(&name) {
                used_shared_regions.insert("caboose");
                shared_regions.insert("caboose".to_owned());
            }
        }

        let extern_regions = toml.extern_regions_for(name, image_name)?;
        let owned_regions = task_allocations[name].iter()
            .chain(extern_regions.iter())
            .map(|(out_name, range)| {
                // Look up region for this image
                let mut regions = toml.outputs[out_name].iter()
                    .filter(|o| &o.name == image_name);
                let out = regions.next().expect("no region for name");
                if regions.next().is_some() {
                    bail!("multiple {} regions for name {}", out_name, image_name);
                }
                let size = range.end - range.start;
                if p2_required && !size.is_power_of_two() {
                    bail!("memory region for task '{}' output '{}' is required to be \
                           a power of two, but has size {}",
                           name, out_name, size);
                }

                Ok((out_name.to_string(), build_kconfig::RegionConfig {
                    base: range.start,
                    size,
                    attributes: build_kconfig::RegionAttributes {
                        read: out.read,
                        write: out.write,
                        execute: out.execute,
                        special_role: if out.dma {
                            Some(build_kconfig::SpecialRole::Dma)
                        } else {
                            None
                        },
                    },
                }))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;

        tasks.push(build_kconfig::TaskConfig {
            owned_regions,
            shared_regions,
            entry_point: build_kconfig::OwnedAddress {
                region_name: "flash".to_string(),
                offset: entry_offset,
            },
            initial_stack: build_kconfig::OwnedAddress {
                region_name: "ram".to_string(),
                offset: stacksize,
            },
            priority: task.priority,
            start_at_boot: task.start,
        });

        // Interrupts.
        for (irq_str, notification) in &task.interrupts {
            // The irq_str should be a reference to a peripheral.
            let irq_num: u32 =
                // Peripheral references are of the form "P.I", where P is
                // the peripheral name and I is the name of one of the
                // peripheral's defined interrupts.
                if let Some(dot_pos) = irq_str.bytes().position(|b| b == b'.') {
                    let (pname, iname) = irq_str.split_at(dot_pos);
                    let iname = &iname[1..];
                    let periph =
                        toml.peripherals.get(pname).ok_or_else(|| {
                            anyhow!(
                                "task {} IRQ {} references peripheral {}, \
                                 which does not exist.",
                                name,
                                irq_str,
                                pname,
                            )
                        })?;
                    periph.interrupts.get(iname).ok_or_else(|| {
                        anyhow!(
                            "task {} IRQ {} references interrupt {} \
                             on peripheral {}, but that interrupt name \
                             not defined for that peripheral.",
                            name,
                            irq_str,
                            iname,
                            pname,
                        )
                    }).cloned()?
                } else {
                    bail!(
                        "task {}: IRQ name {} does not match any \
                         known peripheral interrupt.",
                        name,
                        irq_str,
                    );
                };

            if !notification.ends_with("-irq") {
                bail!(
                    "peripheral interrupt {notification} in {name} \
                     must end in `-irq`"
                );
            }
            let mask = task
                .notification_mask(notification)
                .context(format!("when building {name}"))?;
            assert_eq!(mask.count_ones(), 1);
            irqs.insert(
                irq_num,
                build_kconfig::InterruptConfig {
                    task_index: i,
                    notification: mask,
                },
            );
        }
    }

    // Pare down the list of shared regions.
    flat_shared.retain(|name, _v| used_shared_regions.contains(name.as_str()));

    Ok(build_kconfig::KernelConfig {
        irqs,
        tasks,
        shared_regions: flat_shared,
    })
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

        // We use this function to re-load an ELF file after we've modified
        // it. Don't check for overlap if this happens.
        if !output.contains_key(&addr) {
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
        inner.set_comment(format!(
            "hubris build archive v{HUBRIS_ARCHIVE_VERSION}"
        ));
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
            .start_file(zip_path.as_ref().to_slash().unwrap(), self.opts)?;
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
            .start_file(zip_path.as_ref().to_slash().unwrap(), self.opts)?;
        self.inner.write_all(contents.as_ref().as_bytes())?;
        Ok(())
    }

    /// Creates a binary file in the archive at `zip_path` with `contents`.
    fn binary(
        &mut self,
        zip_path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<()> {
        self.inner
            .start_file(zip_path.as_ref().to_slash().unwrap(), self.opts)?;
        self.inner.write_all(contents.as_ref())?;
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

fn cargo_clean(names: &[&str], target: &str) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("clean");
    println!("cleaning {:?}", names);
    for name in names {
        cmd.arg("-p").arg(name);
    }
    cmd.arg("--release").arg("--target").arg(target);

    let status = cmd
        .status()
        .context(format!("failed to cargo clean ({:?})", cmd))?;

    if !status.success() {
        bail!("command failed, see output for details");
    }

    Ok(())
}

fn resolve_task_slots(
    cfg: &PackageConfig,
    task_name: &str,
    image_name: &str,
) -> Result<()> {
    use scroll::{Pread, Pwrite};

    let task_toml = &cfg.toml.tasks[task_name];

    let task_bin = cfg.img_file(&task_name, image_name);
    let in_task_bin = std::fs::read(&task_bin)?;
    let elf = goblin::elf::Elf::parse(&in_task_bin)?;

    let mut out_task_bin = in_task_bin.clone();

    for entry in task_slot::get_task_slot_table_entries(&in_task_bin, &elf)? {
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
            match cfg.toml.tasks.get_index_of(target_task_name) {
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

        if cfg.verbose {
            println!(
                "Task '{}' task_slot '{}' changed from task index {:#x} to task index {:#x}",
                task_name, entry.slot_name, in_task_idx, target_task_idx
            );
        }
    }

    Ok(std::fs::write(task_bin, out_task_bin)?)
}
