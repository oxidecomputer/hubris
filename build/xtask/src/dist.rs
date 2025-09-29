// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use atty::Stream;
use indexmap::IndexMap;
use lpc55_rom_data::FLASH_PAGE_SIZE as LPC55_FLASH_PAGE_SIZE;
use multimap::MultiMap;
use path_slash::{PathBufExt, PathExt};
use sha3::{Digest, Sha3_256};
use zerocopy::IntoBytes;

use crate::{
    caboose_pos,
    config::{BuildConfig, CabooseConfig, Config},
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
///
/// # Changelog
/// Version 10 requires Humility to be aware of the `handoff` kernel feature,
/// which lets the RoT inform the SP when measurements have been taken.  If
/// Humility is unaware of this feature, the SP will reset itself repeatedly,
/// which interferes with subsequent programming of auxiliary flash.
const HUBRIS_ARCHIVE_VERSION: u32 = 10;

/// `PackageConfig` contains a bundle of data that's commonly used when
/// building a full app image, grouped together to avoid passing a bunch
/// of individual arguments to functions.
///
/// It should be trivial to calculate and kept constant during the build;
/// mutable build information should be accumulated elsewhere.
pub struct PackageConfig {
    /// Directory containing the `app.toml` file being built
    ///
    /// Files specified within the manifest are relative to this directory
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

        // We require a board file in the `boards` directory
        let board_path =
            Path::new("boards").join(format!("{}.toml", toml.board));
        if !board_path.exists() {
            bail!("Failed to find {:?}", board_path);
        }

        Ok(Self {
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
            // https://github.com/rust-lang/cargo/blob/5dfdd59/src/cargo/core/source_id.rs#L755-L794
            //
            // It depends on system architecture, so this won't work on (for example)
            // a Raspberry Pi, but the only downside is that panic messages will
            // be longer.
            let cargo_registry = cargo_home
                .join("registry")
                .join("src")
                .join("github.com-1ecc6299db9ec823");
            remap_paths.insert(cargo_registry, "/crates.io");
            // If Cargo uses the sparse registry (stabilized since ~1.72) it caches fetched crates
            // in a slightly different path. Remap that one as well.
            //
            // This path has the same canonical-ish properties as above.
            let cargo_sparse_registry = cargo_home
                .join("registry")
                .join("src")
                .join("index.crates.io-1949cf8c6b5b557f");
            remap_paths.insert(cargo_sparse_registry, "/crates.io");
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

/// Module to prevent people from messing with invariants of checked types
mod checked_types {
    use super::*;

    /// Simple data structure to store a set of guaranteed-contiguous ranges
    ///
    /// This will panic if you violate that constraint!
    #[derive(Debug, Clone, Default, Hash)]
    pub struct ContiguousRanges(Vec<Range<u32>>);
    impl ContiguousRanges {
        pub fn new(r: Range<u32>) -> Self {
            Self(vec![r])
        }
        pub fn iter(&self) -> impl Iterator<Item = &Range<u32>> {
            self.0.iter()
        }
        pub fn start(&self) -> u32 {
            self.0.first().unwrap().start
        }
        pub fn end(&self) -> u32 {
            self.0.last().unwrap().end
        }
        pub fn contains(&self, v: &u32) -> bool {
            (self.start()..self.end()).contains(v)
        }
        pub fn push(&mut self, r: Range<u32>) {
            if let Some(t) = &self.0.last() {
                assert_eq!(t.end, r.start, "ranges must be contiguous");
            }
            self.0.push(r)
        }
    }

    impl<'a> IntoIterator for &'a ContiguousRanges {
        type Item = &'a Range<u32>;
        type IntoIter = std::slice::Iter<'a, Range<u32>>;
        fn into_iter(self) -> Self::IntoIter {
            self.0.iter()
        }
    }

    /// Simple wrapper data structure that enforces that values are decreasing
    ///
    /// Each value must be the same or smaller than the previous value
    #[derive(Debug)]
    pub struct OrderedVecDeque {
        data: VecDeque<u32>,
    }
    impl Default for OrderedVecDeque {
        fn default() -> Self {
            Self::new()
        }
    }
    impl OrderedVecDeque {
        pub fn new() -> Self {
            Self {
                data: VecDeque::new(),
            }
        }
        pub fn iter(&self) -> impl Iterator<Item = &u32> {
            self.data.iter()
        }
        pub fn into_iter(self) -> impl DoubleEndedIterator<Item = u32> {
            self.data.into_iter()
        }
        pub fn front(&self) -> Option<&u32> {
            self.data.front()
        }
        pub fn pop_front(&mut self) -> Option<u32> {
            self.data.pop_front()
        }
        pub fn push_front(&mut self, v: u32) {
            if let Some(f) = self.front() {
                assert!(v >= *f);
            }
            self.data.push_front(v)
        }
        pub fn back(&self) -> Option<&u32> {
            self.data.back()
        }
        pub fn push_back(&mut self, v: u32) {
            if let Some(f) = self.back() {
                assert!(v <= *f);
            }
            self.data.push_back(v)
        }
    }
    impl From<OrderedVecDeque> for VecDeque<u32> {
        fn from(v: OrderedVecDeque) -> Self {
            v.data
        }
    }
}
// Republish these types for widespread availability
pub use checked_types::{ContiguousRanges, OrderedVecDeque};

pub fn package(
    verbose: bool,
    edges: bool,
    app_toml: &Path,
    tasks_to_build: Option<Vec<String>>,
    dirty_ok: bool,
    caboose_args: super::CabooseArgs,
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

    // Build a set of requests for the memory allocator
    let mut task_reqs = HashMap::new();
    for (t, sz) in task_sizes {
        let n = sz.len()
            + cfg
                .toml
                .extern_regions_for(t, &cfg.toml.image_names[0])
                .unwrap()
                .len()
            + cfg.toml.tasks.get(t).unwrap().uses.len()
            + cfg
                .toml
                .caboose
                .as_ref()
                .map(|c| c.tasks.contains(&t.to_string()))
                .unwrap_or(false) as usize;

        task_reqs.insert(
            t,
            TaskRequest {
                memory: sz,
                spare_regions: 7 - n,
            },
        );
    }

    // Allocate memories.
    let allocated =
        allocate_all(&cfg.toml, &task_reqs, cfg.toml.caboose.as_ref())?;

    for image_name in &cfg.toml.image_names {
        // Build each task.
        let mut all_output_sections = BTreeMap::default();

        std::fs::create_dir_all(cfg.img_dir(image_name))?;
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
        // Same check for the kernel.  This may be overly conservative, because
        // the kernel is special, but we can always make it less strict later.
        for r in &cfg.toml.kernel.extern_regions {
            if let Some(v) = alloc_regions.get(r) {
                bail!(
                    "cannot use region '{r}' as extern region in \
                    the kernel because it's used as a normal region by \
                    [{}]",
                    v.join(", ")
                );
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
                    // Link tasks regardless of whether they have changed,
                    // because we don't want to track changes in the other
                    // linker input (task-link.x, memory.x, table.ld, etc)
                    link_task(&cfg, name, image_name, allocs)?;
                    task_entry_point(&cfg, name, image_name)
                } else {
                    // Dummy entry point
                    Ok(allocs.tasks[name]["flash"].start())
                };
                ep.map(|ep| (name.clone(), ep))
            })
            .collect::<Result<_, _>>()?;

        // Check stack sizes and resolve task slots in our linked files
        let mut possible_stack_overflow = vec![];
        for task_name in cfg.toml.tasks.keys() {
            if tasks_to_build.contains(task_name.as_str()) {
                if task_can_overflow(&cfg.toml, task_name, verbose)? {
                    possible_stack_overflow.push(task_name);
                }

                resolve_task_slots(&cfg, task_name, image_name)?;
            }
        }
        if !possible_stack_overflow.is_empty() {
            bail!(
                "tasks may overflow: {possible_stack_overflow:?}; \
                 see logs above"
            );
        }

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

            for name in cfg.toml.tasks.keys() {
                if tasks_to_build.contains(name.as_str()) {
                    resolve_caboose_pos(
                        &cfg,
                        name,
                        image_name,
                        caboose_range.start + 4,
                        caboose_range.end - 4,
                    )?;
                }
            }
        }

        // Now that we've resolved the task slots and caboose position, we're
        // done making low-level modifications to ELF files on disk.  We'll load
        // all of their data into our `all_output_sections` variable, which is
        // used as the source of truth for the final (combined) files.
        for task_name in cfg.toml.tasks.keys() {
            if tasks_to_build.contains(task_name.as_str()) {
                load_task_flash(
                    &cfg,
                    task_name,
                    image_name,
                    &mut all_output_sections,
                )?;
            }
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

        // Post-build modifications: populate the caboose if requested
        if cfg.toml.caboose.is_some() {
            let mut archive = hubtools::RawHubrisArchive::load(&archive_name)
                .context("loading archive with hubtools")?;
            if let Some(ref vers) = caboose_args.version_override {
                println!("note: asked to override caboose `VERS` to {vers:?}");
            }
            // The Git hash is included in the default caboose under the key
            // `GITC`, so we don't include it in the pseudo-version.
            archive
                .write_default_caboose(caboose_args.version_override.as_ref())
                .context("writing caboose into archive")?;
            archive.overwrite().context("overwriting archive")?;
        } else if let Some(ref vers) = caboose_args.version_override {
            // If there's no caboose, the version override does nothing --- make
            // sure the user realizes that.
            eprintln!(
                "warning: ignoring overridden caboose version \
                 (HUBRIS_CABOOSE_VERS={vers:?}) as {} does not have a \
                 `[caboose]` section!",
                app_toml.display()
            );
        }

        // Post-build modifications: sign the image if requested
        if let Some(signing) = &cfg.toml.signing {
            let mut archive = hubtools::RawHubrisArchive::load(&archive_name)
                .context("loading archive with hubtools")?;
            let priv_key_rel_path = signing
                .certs
                .private_key
                .clone()
                .context("missing private key path")?;
            let private_key = lpc55_sign::cert::read_rsa_private_key(
                &cfg.app_src_dir.join(priv_key_rel_path),
            )
            .with_context(|| {
                format!(
                    "could not read private key {:?}",
                    signing.certs.private_key
                )
            })?;

            // Certificate paths are relative to the app.toml.  Resolve them
            // before attempting to read them.
            let root_cert_abspaths: Vec<PathBuf> = signing
                .certs
                .root_certs
                .iter()
                .map(|c| cfg.app_src_dir.join(c))
                .collect();
            let root_certs = lpc55_sign::cert::read_certs(&root_cert_abspaths)?;

            let signing_cert_abspaths: Vec<PathBuf> = signing
                .certs
                .signing_certs
                .iter()
                .map(|c| cfg.app_src_dir.join(c))
                .collect();
            let signing_certs =
                lpc55_sign::cert::read_certs(&signing_cert_abspaths)?;

            archive.sign(
                signing_certs,
                root_certs.clone(),
                &private_key,
                0, // execution address (TODO)
            )?;

            archive.overwrite()?;
        }

        if cfg.toml.fwid {
            write_fwid(&cfg, image_name, &flash, &archive_name)?;
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

// generate file with hash of expected flash contents
fn write_fwid(
    cfg: &PackageConfig,
    image_name: &str,
    flash: &Range<u32>,
    archive_name: &PathBuf,
) -> Result<()> {
    let mut archive = hubtools::RawHubrisArchive::load(archive_name)
        .context("loading archive with hubtools")?;

    let bin = archive
        .extract_file("img/final.bin")
        .context("extracting final.bin after signing & caboosing")?;

    let chip_name = Path::new(&cfg.toml.chip);

    // determine length of padding
    let pad = match chip_name.file_name().and_then(OsStr::to_str) {
        Some("lpc55") => {
            // Flash is programmed in 512 blocks. If the final block is not
            // filled, it is padded with 0xff's. Unwritten flash pages cannot
            // be read and are not included in the FWID calculation.
            LPC55_FLASH_PAGE_SIZE - bin.len() % LPC55_FLASH_PAGE_SIZE
        }
        Some("stm32h7") => {
            // all unprogrammed flash is read as 0xff
            flash.end as usize - flash.start as usize - bin.len()
        }
        Some(c) => {
            bail!("no FWID algorithm defined for chip: \"{}\"", c)
        }
        None => bail!("Failed to get file name of {}", chip_name.display()),
    };

    let mut sha = Sha3_256::new();
    sha.update(&bin);

    if pad != 0 {
        sha.update(vec![0xff_u8; pad])
    }

    let digest = sha.finalize();

    // after we've appended a newline fwid is immutable
    let mut fwid = hex::encode(digest);
    writeln!(fwid).context("appending newline to FWID")?;
    let fwid = fwid;

    // the archive already exists so we write the FWID to the same path in
    // the build output and archive to keep the two consistent
    fs::write(cfg.img_file("final.fwid", image_name), &fwid)
        .context("writing FWID to build output")?;
    archive
        .add_file("img/final.fwid", fwid.as_bytes())
        .context("writing FWID to archive")?;

    archive.overwrite()?;

    Ok(())
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
        cfg.img_file(cfg.toml.archive_name(image_name), image_name);
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
    archive
        .text(
            "git-rev",
            format!("{}{}", git_rev, if git_dirty { "-dirty" } else { "" }),
        )
        .context("failed writing `git-rev`")?;

    archive
        .text("image-name", image_name)
        .context("failed writing `image-name`")?;
    archive.text("app.toml", &cfg.toml.app_config)?;

    let chip_dir = cfg.app_src_dir.join(cfg.toml.chip.clone());

    // Generate a synthetic `chip.toml` by serializing our peripheral map,
    // because we may have added addition FMC peripherals.
    archive
        .text(
            "chip.toml",
            toml::to_string(&cfg.toml.peripherals)
                .context("could not serialize chip.toml")?,
        )
        .context("could not write chip.toml")?;

    archive
        .text(
            "memory.toml",
            toml::to_string(&cfg.toml.outputs)
                .context("could not serialize memory.toml")?,
        )
        .context("could not write memory.toml")?;

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
    {
        let config = crate::flash::config(&cfg.toml.board)?;
        archive.text(
            img_dir.join("flash.ron"),
            ron::ser::to_string_pretty(
                &config,
                ron::ser::PrettyConfig::default(),
            )?,
        )?;
    }

    let debug_dir = PathBuf::from("debug");

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
            "Running `cargo xtask build` without specifying tasks has no \
            effect.\nDid you mean to run `cargo xtask dist`?"
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

/// Checks whether the given task can overflow its stack
///
/// False negatives are possible if the deepest posssible stack uses dynamic
/// dispatch or function pointers; false positives are technically possible but
/// unlikely if there's a logically unreachable section of the call graph.
fn task_can_overflow(
    toml: &Config,
    task_name: &str,
    verbose: bool,
) -> Result<bool> {
    let max_stack = get_max_stack(toml, task_name, verbose)?;
    let max_depth: u64 = max_stack.iter().map(|(d, _)| *d).sum();

    let task_stack_size = toml.tasks[task_name]
        .stacksize
        .unwrap_or_else(|| toml.stacksize.unwrap());
    let can_overflow = max_depth >= task_stack_size as u64;
    if verbose || can_overflow {
        let extra = if can_overflow {
            format!(
                " exceeds task stack size: {max_depth} >= {task_stack_size}"
            )
        } else {
            format!(
                ": {max_depth} bytes \
                (< task stack size of {task_stack_size} bytes)"
            )
        };
        println!("deepest stack for {task_name}{extra}");
        for (frame_size, name) in max_stack {
            let s = format!("[+{frame_size}]");
            println!("  {s:>7} {name}");
        }
        Ok(can_overflow)
    } else {
        Ok(false)
    }
}

/// Estimates the maximum stack size for the given task
///
/// This does not take dynamic function calls into account, which could cause
/// underestimation.  Overestimation is less likely, but still may happen if
/// there are logically impossible call trees (e.g. `A -> B` and `B -> C`, but
/// `B` never calls `C` if called by `A`).
pub fn get_max_stack(
    toml: &Config,
    task_name: &str,
    verbose: bool,
) -> Result<Vec<(u64, String)>> {
    // Open the statically-linked ELF file
    let f = Path::new("target")
        .join(&toml.name)
        .join("dist")
        .join(format!("{task_name}.tmp"));
    let data = std::fs::read(f).context("could not open ELF file")?;
    let elf = goblin::elf::Elf::parse(&data)?;

    // Read the .stack_sizes section, which is an array of
    // `(address: u32, stack size: unsigned leb128)` tuples
    let sizes = crate::elf::get_section_by_name(&elf, ".stack_sizes")
        .context("could not get .stack_sizes")?;
    let mut sizes = &data[sizes.sh_offset as usize..][..sizes.sh_size as usize];
    let mut addr_to_frame_size = BTreeMap::new();
    while !sizes.is_empty() {
        let (addr, rest) = sizes.split_at(4);
        let addr = u32::from_le_bytes(addr.try_into().unwrap());
        sizes = rest;
        let size = leb128::read::unsigned(&mut sizes)?;
        addr_to_frame_size.insert(addr, size);
    }

    // There are `$t` and `$d` symbols which indicate the beginning of text
    // versus data in the `.text` region.  We collect them into a `BTreeMap`
    // here so that we can avoid trying to decode inline data words.
    let mut text_regions = BTreeMap::new();
    for sym in elf.syms.iter() {
        if sym.st_name == 0
            || sym.st_size != 0
            || sym.st_type() != goblin::elf::sym::STT_NOTYPE
        {
            continue;
        }

        let addr = sym.st_value as u32;
        let is_text = match elf.strtab.get_at(sym.st_name) {
            Some("$t") => true,
            Some("$d") => false,
            Some(_) => continue,
            None => {
                bail!("bad symbol in {task_name}: {}", sym.st_name);
            }
        };
        text_regions.insert(addr, is_text);
    }
    let is_code = |addr| {
        let mut iter = text_regions.range(..=addr);
        *iter.next_back().unwrap().1
    };

    // We'll be packing everything into this data structure
    #[derive(Debug)]
    struct FunctionData {
        name: String,
        short_name: String,
        frame_size: Option<u64>,
        calls: BTreeSet<u32>,
    }

    let text = crate::elf::get_section_by_name(&elf, ".text")
        .context("could not get .text")?;

    use capstone::{
        arch::{arm, ArchOperand, BuildsCapstone, BuildsCapstoneExtraMode},
        Capstone, InsnGroupId, InsnGroupType,
    };
    let cs = Capstone::new()
        .arm()
        .mode(arm::ArchMode::Thumb)
        .extra_mode(std::iter::once(arm::ArchExtraMode::MClass))
        .detail(true)
        .build()
        .map_err(|e| anyhow!("failed to initialize disassembler: {e:?}"))?;

    // Disassemble each function, building a map of its call sites
    let mut fns = BTreeMap::new();
    for sym in elf.syms.iter() {
        // We only care about named function symbols here
        if sym.st_name == 0 || !sym.is_function() || sym.st_size == 0 {
            continue;
        }

        let Some(name) = elf.strtab.get_at(sym.st_name) else {
            bail!("bad symbol in {task_name}: {}", sym.st_name);
        };

        // Clear the lowest bit, which indicates that the function contains
        // thumb instructions (always true for our systems!)
        let val = sym.st_value & !1;
        let base_addr = val as u32;

        // Get the text region for this function
        let offset = (val - text.sh_addr + text.sh_offset) as usize;
        let text = &data[offset..][..sym.st_size as usize];

        // Split the text region into instruction-only chunks
        let mut chunks = vec![];
        let mut chunk = None;
        for (i, b) in text.iter().enumerate() {
            let addr = base_addr + i as u32;
            if is_code(addr) {
                chunk.get_or_insert((addr, vec![])).1.push(*b);
            } else {
                chunks.extend(chunk.take());
            }
        }
        chunks.extend(chunk); // don't forget the trailing chunk!

        let frame_size = addr_to_frame_size.get(&base_addr).copied();
        let mut calls = BTreeSet::new();
        for (addr, chunk) in chunks {
            let instrs = cs
                .disasm_all(&chunk, addr.into())
                .map_err(|e| anyhow!("disassembly failed: {e:?}"))?;
            for (i, instr) in instrs.iter().enumerate() {
                let detail = cs.insn_detail(instr).map_err(|e| {
                    anyhow!("could not get instruction details: {e}")
                })?;

                // Detect tail calls, which are jumps at the final instruction
                // when the function itself has no stack frame.
                let can_tail = frame_size == Some(0) && i == instrs.len() - 1;
                if detail.groups().iter().any(|g| {
                    g == &InsnGroupId(InsnGroupType::CS_GRP_CALL as u8)
                        || (g == &InsnGroupId(InsnGroupType::CS_GRP_JUMP as u8)
                            && can_tail)
                }) {
                    let arch = detail.arch_detail();
                    let ops = arch.operands();
                    let op = ops.last().unwrap_or_else(|| {
                        panic!("missing operand!");
                    });

                    let ArchOperand::ArmOperand(op) = op else {
                        panic!("bad operand type: {op:?}");
                    };
                    // We can't resolve indirect calls, alas
                    let arm::ArmOperandType::Imm(target) = op.op_type else {
                        continue;
                    };
                    let target = u32::try_from(target).unwrap();

                    // Avoid recursive calls into the same function (or midway
                    // into the function, which is a thing we've seen before!
                    // it's weird!)
                    if !(base_addr..base_addr + sym.st_size as u32)
                        .contains(&target)
                    {
                        calls.insert(target);
                    }
                }
            }
        }

        let name = rustc_demangle::demangle(name).to_string();

        // Strip the trailing hash from the name for ease of printing
        let short_name = if let Some(i) = name.rfind("::") {
            &name[..i]
        } else {
            &name
        }
        .to_owned();

        fns.insert(
            base_addr,
            FunctionData {
                name,
                short_name,
                frame_size,
                calls,
            },
        );
    }

    fn recurse(
        call_stack: &mut Vec<u32>,
        recurse_depth: usize,
        mut stack_depth: u64,
        fns: &BTreeMap<u32, FunctionData>,
        deepest: &mut Option<(u64, Vec<u32>)>,
        verbose: bool,
    ) {
        let addr = *call_stack.last().unwrap();
        let Some(f) = fns.get(&addr) else {
            panic!("found jump to unknown function at {call_stack:08x?}");
        };
        let frame_size = f.frame_size.unwrap_or(0);
        stack_depth += frame_size;
        if verbose {
            let indent = recurse_depth * 2;
            println!(
                "  {:indent$}{addr:08x}: {} [+{frame_size} => {stack_depth}]",
                "",
                f.short_name,
                indent = indent
            );
        }

        if deepest
            .as_ref()
            .map(|(max_depth, _)| stack_depth > *max_depth)
            .unwrap_or(true)
        {
            *deepest = Some((stack_depth, call_stack.to_owned()));
        }
        for j in &f.calls {
            if call_stack.contains(j) {
                // Skip recursive / mutually recursive calls, because we can't
                // reason about them.
                continue;
            } else {
                call_stack.push(*j);
                recurse(
                    call_stack,
                    recurse_depth + 1,
                    stack_depth,
                    fns,
                    deepest,
                    verbose,
                );
                call_stack.pop();
            }
        }
    }

    // Find stack sizes by traversing the graph
    if verbose {
        println!("finding stack sizes for {task_name}");
    }
    let start_addr = fns
        .iter()
        .find(|(_addr, v)| v.name.as_str() == "_start")
        .map(|(addr, _v)| *addr)
        .ok_or_else(|| anyhow!("could not find _start"))?;
    let mut deepest = None;
    recurse(&mut vec![start_addr], 0, 0, &fns, &mut deepest, verbose);

    // Check against our configured task stack size
    let Some((_max_depth, max_stack)) = deepest else {
        unreachable!("must have at least one call stack");
    };

    let mut out = vec![];
    for m in max_stack {
        let f = fns.get(&m).unwrap();
        let name = &f.short_name;
        out.push((f.frame_size.unwrap_or(0), name.clone()));
    }
    Ok(out)
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
        format!("{}.elf", name),
        format!("{}/{}", image_name, name),
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
        .map(|(name, r)| (name, ContiguousRanges::new(r)))
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
    link(cfg, format!("{}.elf", name), format!("{}.tmp", name))
}

fn task_size<'a>(
    cfg: &'a PackageConfig,
    name: &str,
) -> Result<IndexMap<&'a str, u64>> {
    let task = &cfg.toml.tasks[name];
    let stacksize = task.stacksize.or(cfg.toml.stacksize).unwrap();
    load_task_size(&cfg.toml, name, stacksize)
}

/// Finds the entry point of the given task
fn task_entry_point(
    cfg: &PackageConfig,
    name: &str,
    image_name: &str,
) -> Result<u32> {
    get_elf_entry_point(&cfg.img_file(name, image_name))
}

/// Populates `all_output_sections` and checks flash size
fn load_task_flash(
    cfg: &PackageConfig,
    name: &str,
    image_name: &str,
    all_output_sections: &mut BTreeMap<u32, LoadSegment>,
) -> Result<()> {
    let task_toml = &cfg.toml.tasks[name];
    let mut symbol_table = BTreeMap::default();
    let flash = load_elf(
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
    Ok(())
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

    let extern_regions = cfg.toml.kernel_extern_regions(image_name)?;
    generate_kernel_linker_script(
        "memory.x",
        &allocs.kernel,
        cfg.toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &cfg.toml.all_regions("flash".to_string())?,
        &extern_regions,
        image_name,
    )?;

    fs::copy("build/kernel-link.x", "target/link.x")?;

    let image_id = image_id.finish();

    let flash_outputs = if let Some(o) = cfg.toml.outputs.get("flash") {
        ron::ser::to_string(o)?
    } else {
        bail!("no 'flash' output regions defined in config toml");
    };

    // Build the kernel.
    let build_config = cfg.toml.kernel_build_config(
        cfg.verbose,
        &[
            ("HUBRIS_KCONFIG", &kconfig),
            ("HUBRIS_IMAGE_ID", &format!("{}", image_id)),
            ("HUBRIS_FLASH_OUTPUTS", &flash_outputs),
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
            cfg.dist_file("kernel"),
            cfg.img_file("kernel.orig", image_name),
        )?;
        std::fs::copy(
            cfg.img_file("kernel.modified", image_name),
            cfg.img_file("kernel", image_name),
        )?;
    } else {
        std::fs::copy(
            cfg.dist_file("kernel"),
            cfg.img_file("kernel", image_name),
        )?;
    }

    let mut ksymbol_table = BTreeMap::default();
    let kernel_elf_path = cfg.img_file("kernel", image_name);
    let kentry = get_elf_entry_point(&kernel_elf_path)?;
    load_elf(&kernel_elf_path, all_output_sections, &mut ksymbol_table)?;
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
                    total_image_len: len,
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
                bail!(
                    concat!(
                        "Priority inversion: ",
                        "task {} (priority {}) calls into {} (priority {})",
                    ),
                    name,
                    task.priority,
                    callee,
                    p
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
    map: &BTreeMap<String, ContiguousRanges>,
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
    for (name, ranges) in map {
        let mut start = ranges.start();
        let end = ranges.end();
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
    extern_regions: &IndexMap<String, Range<u32>>,
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
    append_extern_regions(&mut linkscr, extern_regions)?;
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

    let remap_path_prefix =
        cfg.remap_paths.iter().fold(String::new(), |mut output, r| {
            let _ = write!(
                output,
                " --remap-path-prefix={}={}",
                r.0.display(),
                r.1
            );
            output
        });
    cmd.env(
        "RUSTFLAGS",
        format!(
            "-C link-arg=-z -C link-arg=common-page-size=0x20 \
             -C link-arg=-z -C link-arg=max-page-size=0x20 \
             -C llvm-args=--enable-machine-outliner=never \
             -Z emit-stack-sizes \
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

            out_bytes.extend(buf[0..num].iter());
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
                std::str::from_utf8(c.get(1).unwrap().as_bytes()).unwrap();
            let task =
                std::str::from_utf8(c.get(3).unwrap().as_bytes()).unwrap();
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
                            .replace('_', "-")
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
    ///
    /// A task may have multiple address ranges in the same memory space for
    /// efficient packing; if this is the case, the addresses will be contiguous
    /// and each individual range will respect MPU requirements.
    pub tasks: BTreeMap<String, BTreeMap<String, ContiguousRanges>>,
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

/// A set of memory requests from a single task
#[derive(Debug, Clone)]
pub struct TaskRequest<'a> {
    /// Memory requests, as a map from memory name -> size
    pub memory: IndexMap<&'a str, u64>,

    /// Number of extra regions available for more efficient packing
    ///
    /// If this is zero, then each request in `memory` can only use 1 region
    pub spare_regions: usize,
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
    task_sizes: &HashMap<&str, TaskRequest>,
    caboose: Option<&CabooseConfig>,
) -> Result<BTreeMap<String, AllocationMap>> {
    // Collect all allocation requests into queues, one per memory type, indexed
    // by allocation size. This is equivalent to required alignment because of
    // the naturally-aligned-power-of-two requirement.
    //
    // We keep kernel and task requests separate so we can always service the
    // kernel first.
    //
    // The task map is: memory name -> task name -> requested regions
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

        let mut task_requests: BTreeMap<&str, IndexMap<&str, OrderedVecDeque>> =
            BTreeMap::new();

        for name in tasks.keys() {
            let req = &task_sizes[name.as_str()];
            for (&mem, &amt) in req.memory.iter() {
                // Right now, flash is most limited, so it gets to use all of
                // our spare regions (if present)
                let n = if mem == "flash" {
                    req.spare_regions + 1
                } else {
                    1
                };
                let bytes = toml.suggest_memory_region_size(name, amt, n);
                if let Some(r) = tasks[name].max_sizes.get(&mem.to_string()) {
                    let total_bytes = bytes.iter().sum::<u64>();
                    if total_bytes > u64::from(*r) {
                        bail!(
                        "task {}: needs {} bytes of {} but max-sizes limits it to {}",
                        name, total_bytes, mem, r);
                    }
                }
                // Convert from u64 -> u32
                let mut bs = OrderedVecDeque::new();
                for b in bytes {
                    bs.push_back(b.try_into().unwrap());
                }
                task_requests
                    .entry(mem)
                    .or_default()
                    .insert(name.as_str(), bs);
            }
        }

        // Okay! Do memory types one by one, fitting kernel first.
        for (region, avail) in &mut free {
            let mut k_req = kernel_requests.get(region.as_str());
            let t_reqs = task_requests.get_mut(region.as_str());
            let mut t_reqs_empty = IndexMap::new();
            allocate_region(
                region,
                toml,
                &mut k_req,
                t_reqs.unwrap_or(&mut t_reqs_empty),
                avail,
                &mut allocs,
            )?;
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

fn allocate_region(
    region: &str,
    toml: &Config,
    k_req: &mut Option<&u32>,
    t_reqs: &mut IndexMap<&str, OrderedVecDeque>,
    avail: &mut Range<u32>,
    allocs: &mut Allocations,
) -> Result<()> {
    // The kernel gets to go first!
    if let Some(&sz) = k_req.take() {
        allocs
            .kernel
            .insert(region.to_string(), allocate_k(region, sz, avail)?);
    }

    while !t_reqs.is_empty() {
        // At this point, we need to find a task that fits based on our existing
        // alignment.  This is tricky, because -- for efficient packing -- we
        // allow tasks to span multiple regions.  For example, a task could look
        // like this:
        //
        //   4444221
        //
        // representing three regions of size 4, 2, 1.
        //
        // Such a task could be placed in two ways:
        //
        //      |4444221 ("forward")
        //   122|4444    ("reverse")
        //      | where this line is the alignment for the largest chunk

        #[derive(Debug)]
        enum Direction {
            Forward,
            Reverse,
        }
        #[derive(Debug)]
        struct Match<'a> {
            gap: u32,
            align: u32,
            size: u32,
            name: &'a str,
            dir: Direction,
        }
        impl<'a> Match<'a> {
            /// Updates our "current best" with new values, if they're better
            ///
            /// Our policy is to rank by
            /// 1) smallest gap required, and then
            /// 2) largest resulting alignment
            /// 3) smallest task size (for backwards compatibility)
            fn update(
                &mut self,
                gap: u32,
                align: u32,
                size: u32,
                name: &'a str,
                dir: Direction,
            ) {
                // Ignore any gap that's < 1/8 of final alignment, since that's
                // "close enough"
                let gap_m = gap.saturating_sub(align / 8);
                let our_gap_m = self.gap.saturating_sub(self.align / 8);
                if gap_m < our_gap_m
                    || (gap_m == our_gap_m && align > self.align)
                    || gap < self.gap
                    || (gap == self.gap && align > self.align)
                    || (gap == self.gap
                        && align == self.align
                        && size < self.size)
                {
                    self.gap = gap;
                    self.align = align;
                    self.size = size;
                    self.name = name;
                    self.dir = dir;
                }
            }
        }

        let mut best = Match {
            gap: u32::MAX,
            align: 0,
            size: u32::MAX,
            name: "",
            dir: Direction::Forward,
        };

        for (&task_name, mem) in t_reqs.iter() {
            let align = toml.task_memory_alignment(*mem.front().unwrap());
            let size_mask = align - 1;

            // Place the chunk using reverse orientation, with padding if it's
            // not aligned.  The alignment (for scoring purposes) is the
            // alignment of the largest region, since that's last in memory.
            let total_size: u32 = mem.iter().sum();
            let base = (avail.start + total_size + size_mask) & !size_mask;
            let gap_reverse = base - avail.start - total_size;
            best.update(
                gap_reverse,
                align,
                total_size,
                task_name,
                Direction::Reverse,
            );

            // Place the chunk using forward orientation, with padding if it's
            // not aligned.  The alignment (for scoring purposes) is the
            // alignment of the last region, since that may be worse than the
            // starting alignment.
            let base = (avail.start + size_mask) & !size_mask;
            let gap_forward = base - avail.start;
            best.update(
                gap_forward,
                toml.task_memory_alignment(*mem.back().unwrap()),
                total_size,
                task_name,
                Direction::Forward,
            );
        }
        let Some(sizes) = t_reqs.remove(best.name) else {
            panic!("could not find a task");
        };

        // Prepare to pack values either forward or reverse
        //
        // At this point, we drop the "values must be ordered" constraint,
        // because we may combine adjacent regions to reduce the total region
        // count.  This could violate the ordering constraint, but is still
        // valid from the MPU's perspective.
        let mut sizes: VecDeque<u32> = match best.dir {
            Direction::Forward => sizes.into(),
            Direction::Reverse => sizes.into_iter().rev().collect(),
        };
        avail.start += best.gap;

        while let Some(mut size) = sizes.pop_front() {
            // When building the size list, we split the largest size to reduce
            // alignment requirements.  Now, we try to merge them again, to
            // reduce the number of regions stored in the kernel's flash.
            //
            // For example, [256, 256, 64] => [512, 64] if the initial position
            // is aligned for a 512-byte region.
            let mut n = sizes.iter().filter(|s| **s == size).count() + 1;
            if n > 1 {
                n &= !1; // only consider an even number of regions
                let possible_align = toml.task_memory_alignment(size * 2);
                if avail.start & (possible_align - 1) == 0 {
                    size *= 2;
                    for _ in 0..n - 1 {
                        sizes.pop_front();
                    }
                    for _ in 0..n / 2 - 1 {
                        sizes.push_front(size);
                    }
                }
            }

            // We do our own alignment management, so assert that we haven't
            // messed it up:
            let align = toml.task_memory_alignment(size);
            assert!(avail.start & (align - 1) == 0);

            allocs
                .tasks
                .entry(best.name.to_string())
                .or_default()
                .entry(region.to_string())
                .or_default()
                .push(allocate_one(region, size, align, avail)?);
        }

        // Check that our allocations are all aligned and contiguous
        let mut prev: Option<Range<u32>> = None;
        for r in &allocs.tasks[best.name][region] {
            if let Some(prev) = prev {
                assert_eq!(prev.end, r.start);
            }
            let size = r.end - r.start;
            assert!(size >= 32); // minimum MPU size
            let align = toml.task_memory_alignment(size);
            assert!(r.start.trailing_zeros() >= align.trailing_zeros());
            prev = Some(r.clone());
        }
    }

    Ok(())
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
    task_allocations: &BTreeMap<String, BTreeMap<String, ContiguousRanges>>,
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
            entry_points[name] - flash.start()
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
            if caboose.tasks.contains(name) {
                used_shared_regions.insert("caboose");
                shared_regions.insert("caboose".to_owned());
            }
        }

        let extern_regions = toml.extern_regions_for(name, image_name)?;
        let mut owned_regions = BTreeMap::new();
        for (out_name, range) in task_allocations[name]
            .iter()
            .flat_map(|(name, chunks)| chunks.iter().map(move |c| (name, c)))
            .chain(extern_regions.iter())
        {
            // Look up region for this image
            let mut regions = toml.outputs[out_name]
                .iter()
                .filter(|o| o.name == image_name);
            let out = regions.next().expect("no region for name");
            if regions.next().is_some() {
                bail!("multiple {out_name} regions for name {image_name}");
            }
            let size = range.end - range.start;
            if p2_required && !size.is_power_of_two() {
                bail!(
                    "memory region for task '{name}' output '{out_name}' \
                     is required to be a power of two, but has size {size}"
                );
            }

            owned_regions
                .entry(out_name.to_string())
                .or_insert(build_kconfig::MultiRegionConfig {
                    base: range.start,
                    sizes: vec![],
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
                })
                .sizes
                .push(size);
        }

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
        features: toml.kernel.features.clone(),
        extern_regions: toml
            .kernel_extern_regions(image_name)?
            .into_iter()
            .collect(),
        irqs,
        tasks,
        shared_regions: flat_shared,
    })
}

fn get_elf_entry_point(input: &Path) -> Result<u32> {
    use goblin::container::Container;

    let file_image = std::fs::read(input)?;
    let elf = goblin::elf::Elf::parse(&file_image)?;

    if elf.header.container()? != Container::Little {
        bail!("where did you get a big-endian image?");
    }
    if elf.header.e_machine != goblin::elf::header::EM_ARM {
        bail!("this is not an ARM file");
    }

    Ok(elf.header.e_entry as u32)
}

fn load_elf(
    input: &Path,
    output: &mut BTreeMap<u32, LoadSegment>,
    symbol_table: &mut BTreeMap<String, u32>,
) -> Result<usize> {
    use goblin::container::Container;
    use goblin::elf::program_header::PT_LOAD;

    let file_image = std::fs::read(input)?;
    let elf = goblin::elf::Elf::parse(&file_image)?;

    // Checked in get_elf_entry_point above, but we'll re-check them here
    assert_eq!(elf.header.container()?, Container::Little);
    assert_eq!(elf.header.e_machine, goblin::elf::header::EM_ARM);

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

    // Return the total allocated flash, allowing the caller to assure that the
    // allocated flash does not exceed the task's required flash
    Ok(flash)
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
                .compression_method(zip::CompressionMethod::Deflated),
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

    let task_bin = cfg.img_file(task_name, image_name);
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

fn resolve_caboose_pos(
    cfg: &PackageConfig,
    task_name: &str,
    image_name: &str,
    start: u32,
    end: u32,
) -> Result<()> {
    use scroll::Pwrite;

    let task_bin = cfg.img_file(task_name, image_name);
    let in_task_bin = std::fs::read(&task_bin)?;
    let elf = goblin::elf::Elf::parse(&in_task_bin)?;

    let mut out_task_bin = in_task_bin.clone();

    if let Some(entry) =
        caboose_pos::get_caboose_pos_table_entry(&in_task_bin, &elf)?
    {
        out_task_bin.pwrite_with::<u32>(
            start,
            entry.caboose_pos_file_offset as usize,
            elf::get_endianness(&elf),
        )?;
        out_task_bin.pwrite_with::<u32>(
            end,
            entry.caboose_pos_file_offset as usize + 4,
            elf::get_endianness(&elf),
        )?;

        if cfg.verbose {
            println!(
                "Task '{task_name}' caboose pos written to {:#x} as \
                ({start:#x}, {end:#x})",
                entry.caboose_pos_address,
            );
        }
    }

    Ok(std::fs::write(task_bin, out_task_bin)?)
}
