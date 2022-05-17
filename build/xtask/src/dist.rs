// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use indexmap::IndexMap;
use path_slash::PathBufExt;
use serde::Serialize;
use termcolor::{Color, ColorSpec, WriteColor};

use crate::{
    config::{
        BuildConfig, Config, Kernel, Output, Peripheral, Signing, Supervisor,
        Task,
    },
    elf, task_slot,
};

use lpc55_sign::{crc_image, signed_image};

/// In practice, applications with active interrupt activity tend to use about
/// 650 bytes of stack. Because kernel stack overflows are annoying, we've
/// padded that a bit.
pub const DEFAULT_KERNEL_STACK: u32 = 1024;

#[derive(Debug, Hash)]
struct LoadSegment {
    source_file: PathBuf,
    data: Vec<u8>,
}

pub fn package(
    verbose: bool,
    edges: bool,
    cfg: &Path,
    tasks_to_build: Option<Vec<String>>,
) -> Result<()> {
    // If we're using filters, we change behavior at the end. Record this in a
    // convenient flag.
    let partial_build = tasks_to_build.is_some();

    let toml = Config::from_file(&cfg)?;

    if !partial_build {
        check_task_priorities(&toml)?;
    }

    let mut out = PathBuf::from("target");
    let buildstamp_file = out.join("buildstamp");

    out.push(&toml.name);
    out.push("dist");

    std::fs::create_dir_all(&out)?;

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

    let mut src_dir = cfg.to_path_buf();
    src_dir.pop();

    let mut memories = IndexMap::new();
    for (name, out) in &toml.outputs {
        if let Some(end) = out.address.checked_add(out.size) {
            memories.insert(name.clone(), out.address..end);
        } else {
            eprintln!(
                "output {}: address {:08x} size {:x} would overflow",
                name, out.address, out.size
            );
            std::process::exit(1);
        }
    }
    for (name, range) in &memories {
        println!("{:<5} = {:#010x}..{:#010x}", name, range.start, range.end);
    }
    let starting_memories = memories.clone();

    // Allocate memories.
    let allocs = allocate_all(&toml.kernel, &toml.tasks, &mut memories)?;

    println!("Used:");
    for (name, new_range) in &memories {
        let orig_range = &starting_memories[name];
        let size = new_range.start - orig_range.start;
        let percent = size * 100 / (orig_range.end - orig_range.start);
        println!("  {:<6} {:#x} ({}%)", format!("{}:", name), size, percent);
    }

    let mut infofile = File::create(out.join("allocations.txt"))?;
    writeln!(infofile, "kernel: {:#x?}", allocs.kernel)?;
    writeln!(infofile, "tasks: {:#x?}", allocs.tasks)?;
    drop(infofile);

    // Build each task.
    let mut all_output_sections = BTreeMap::default();
    let mut entry_points = HashMap::<_, _>::default();

    // if we need to rebuild, we should clean everything before we start building
    if rebuild {
        println!("app.toml has changed; rebuilding all tasks");

        cargo_clean(&toml.kernel.name, &toml.target)?;

        for name in toml.tasks.keys() {
            // this feels redundant, don't we already have the name? consider
            // our supervisor:
            //
            // [tasks.jefe]
            // path = "../task-jefe"
            // name = "task-jefe"
            //
            // the "name" in the key is jefe, but the package name is in
            // tasks.jefe.name, and that's what we need to give to cargo
            let task_toml = &toml.tasks[name];

            cargo_clean(&task_toml.name, &toml.target)?;
        }
    }

    // now that we're clean, update our buildstamp file; any failure to build
    // from here on need not trigger a clean
    std::fs::write(&buildstamp_file, format!("{:x}", toml.buildhash))?;

    // Panic messages in crates have a long prefix; we'll shorten it using
    // the --remap-path-prefix argument to reduce message size.  We'll remap
    // local (Hubris) crates to /hubris, crates.io to /crates.io, and git
    // dependencies to /git
    let remap_paths = {
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
        remap_paths
    };

    // If there is a bootloader, build it first as there may be dependencies
    // for applications
    if let Some(bootloader) = toml.bootloader.as_ref() {
        if rebuild {
            cargo_clean(&bootloader.name, &toml.target)?;
        }

        let mut bootloader_memory = IndexMap::new();
        let flash = memories.get("bootloader_flash").unwrap();
        let ram = memories.get("bootloader_ram").unwrap();
        let sram = memories.get("bootloader_sram").unwrap();
        let image_flash = if let Some(end) = bootloader
            .imagea_flash_start
            .checked_add(bootloader.imagea_flash_size)
        {
            bootloader.imagea_flash_start..end
        } else {
            eprintln!("image flash size is incorrect");
            std::process::exit(1);
        };
        let image_ram = if let Some(end) = bootloader
            .imagea_ram_start
            .checked_add(bootloader.imagea_ram_size)
        {
            bootloader.imagea_ram_start..end
        } else {
            eprintln!("image ram size is incorrect");
            std::process::exit(1);
        };

        bootloader_memory.insert(String::from("FLASH"), flash.clone());
        bootloader_memory.insert(String::from("RAM"), ram.clone());
        bootloader_memory.insert(String::from("SRAM"), sram.clone());
        bootloader_memory
            .insert(String::from("IMAGEA_FLASH"), image_flash.clone());
        bootloader_memory.insert(String::from("IMAGEA_RAM"), image_ram.clone());

        generate_bootloader_linker_script(
            "memory.x",
            &bootloader_memory,
            Some(&bootloader.sections),
        );

        fs::copy("build/kernel-link.x", "target/link.x")?;

        let build_config = toml.bootloader_build_config(verbose).unwrap();
        build(
            build_config,
            out.join(&bootloader.name),
            edges,
            &remap_paths,
        )?;

        // Need a bootloader binary for signing
        objcopy_translate_format(
            "elf32-littlearm",
            &out.join(&bootloader.name),
            "binary",
            &out.join("bootloader.bin"),
        )?;

        if let Some(signing) = toml.signing.get("bootloader") {
            do_sign_file(
                signing,
                &out,
                &src_dir,
                "bootloader",
                0,
                &starting_memories,
            )?;
        }

        // We need to get the absolute symbols for the non-secure application
        // to call into the secure application. The easiest approach right now
        // is to generate the table in a separate section, objcopy just that
        // section and then re-insert those bits into the application section
        // via linker.

        objcopy_grab_binary(
            "elf32-littlearm",
            &out.join(&bootloader.name),
            &out.join("addr_blob.bin"),
        )?;

        let mut f = std::fs::File::open(&out.join("addr_blob.bin"))?;

        let mut bytes = Vec::new();

        f.read_to_end(&mut bytes)?;

        let mut linkscr =
            File::create(Path::new(&format!("target/table.ld"))).unwrap();

        for b in bytes {
            writeln!(linkscr, "BYTE({:#x})", b).unwrap();
        }

        drop(linkscr);
    } else {
        // Just create a new empty file
        File::create(Path::new(&format!("target/table.ld"))).unwrap();
    }

    // Quick sanity-check if we're trying to build individual tasks which
    // aren't present in the app.toml, or ran `cargo xtask build ...` without
    // any specified tasks.
    if let Some(included_names) = &tasks_to_build {
        if included_names.is_empty() {
            bail!(
                "Running `cargo xtask build` without specifying tasks has no effect.
Did you mean to run `cargo xtask dist`?"
            );
        }
        let all_tasks = toml.tasks.keys().collect::<Vec<_>>();
        for name in included_names {
            if !all_tasks.contains(&name) {
                bail!(toml.task_name_suggestion(name));
            }
        }
    }

    for name in toml.tasks.keys() {
        // Implement task name filter. If we're only building a subset of tasks,
        // skip the other ones here.
        if let Some(included_names) = &tasks_to_build {
            if !included_names.contains(name) {
                continue;
            }
        }
        let task_toml = &toml.tasks[name];

        generate_task_linker_script(
            "memory.x",
            &allocs.tasks[name],
            Some(&task_toml.sections),
            task_toml.stacksize.or(toml.stacksize).ok_or_else(|| {
                anyhow!(
                    "{}: no stack size specified and there is no default",
                    name
                )
            })?,
        )
        .context(format!("failed to generate linker script for {}", name))?;

        fs::copy("build/task-link.x", "target/link.x")?;

        let build_config = toml.task_build_config(name, verbose).unwrap();
        build(build_config, out.join(name), edges, &remap_paths)
            .context(format!("failed to build {}", name))?;

        resolve_task_slots(name, &toml.tasks, &out.join(name), verbose)?;

        let mut symbol_table = BTreeMap::default();
        let (ep, flash) = load_elf(
            &out.join(name),
            &mut all_output_sections,
            &mut symbol_table,
        )?;

        if flash > task_toml.requires["flash"] as usize {
            bail!(
                "{} has insufficient flash: specified {} bytes, needs {}",
                task_toml.name,
                task_toml.requires["flash"],
                flash
            );
        }

        entry_points.insert(name.clone(), ep);
    }

    // If we've done a partial build, we can't do the rest because we're missing
    // required information, so, escape.
    if partial_build {
        return Ok(());
    }

    let mut image_id = fnv::FnvHasher::default();
    all_output_sections.hash(&mut image_id);

    // Format the descriptors for the kernel build.
    let kconfig = make_descriptors(
        &toml.target,
        &toml.tasks,
        &toml.peripherals,
        toml.supervisor.as_ref(),
        &allocs.tasks,
        toml.stacksize,
        &toml.outputs,
        &entry_points,
        &toml.extratext,
    )?;
    let kconfig = ron::ser::to_string(&kconfig)?;

    kconfig.hash(&mut image_id);
    allocs.hash(&mut image_id);

    generate_kernel_linker_script(
        "memory.x",
        &allocs.kernel,
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
    )?;

    fs::copy("build/kernel-link.x", "target/link.x")?;

    let image_id = image_id.finish();

    // Build the kernel.
    let build_config = toml.kernel_build_config(
        verbose,
        &[
            ("HUBRIS_KCONFIG", &kconfig),
            ("HUBRIS_IMAGE_ID", &format!("{}", image_id)),
        ],
    );
    build(build_config, out.join("kernel"), edges, &remap_paths)?;

    let mut ksymbol_table = BTreeMap::default();
    let (kentry, _) = load_elf(
        &out.join("kernel"),
        &mut all_output_sections,
        &mut ksymbol_table,
    )?;

    // Write a map file, because that seems nice.
    let mut mapfile = File::create(&out.join("map.txt"))?;
    writeln!(mapfile, "ADDRESS  END          SIZE FILE")?;
    for (base, sec) in &all_output_sections {
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
    drop(mapfile);

    // Generate combined SREC, which is our source of truth for combined images.
    write_srec(&all_output_sections, kentry, &out.join("combined.srec"))?;

    // Convert SREC to other formats for convenience.
    objcopy_translate_format(
        "srec",
        &out.join("combined.srec"),
        "elf32-littlearm",
        &out.join("combined.elf"),
    )?;
    objcopy_translate_format(
        "srec",
        &out.join("combined.srec"),
        "ihex",
        &out.join("combined.ihex"),
    )?;
    objcopy_translate_format(
        "srec",
        &out.join("combined.srec"),
        "binary",
        &out.join("combined.bin"),
    )?;

    if let Some(signing) = toml.signing.get("combined") {
        match ksymbol_table.get("HEADER") {
            None =>  bail!("Didn't find header symbol -- does the image need a placeholder?"),
            Some(_) => ()
        }

        do_sign_file(
            signing,
            &out,
            &src_dir,
            "combined",
            *ksymbol_table.get("__header_start").unwrap(),
            &starting_memories,
        )?;
    }

    // Okay we now have signed hubris image and signed bootloader
    // Time to combine the two!
    if let Some(bootloader) = toml.bootloader.as_ref() {
        let file_image = std::fs::read(&out.join(&bootloader.name))?;
        let elf = goblin::elf::Elf::parse(&file_image)?;

        let bootloader_entry = elf.header.e_entry as u32;

        let bootloader_fname =
            if let Some(signing) = toml.signing.get("bootloader") {
                format!("bootloader_{}.bin", signing.method)
            } else {
                "bootloader.bin".into()
            };

        let hubris_fname = if let Some(signing) = toml.signing.get("combined") {
            format!("combined_{}.bin", signing.method)
        } else {
            "combined.bin".into()
        };

        let bootloader = toml.outputs.get("bootloader_flash").unwrap().address;
        let flash = toml.outputs.get("flash").unwrap().address;
        smash_bootloader(
            &out.join(bootloader_fname),
            bootloader,
            &out.join(hubris_fname),
            flash,
            bootloader_entry,
            &out.join("final.srec"),
        )?;

        objcopy_translate_format(
            "srec",
            &out.join("final.srec"),
            "elf32-littlearm",
            &out.join("final.elf"),
        )?;

        objcopy_translate_format(
            "srec",
            &out.join("final.srec"),
            "ihex",
            &out.join("final.ihex"),
        )?;

        objcopy_translate_format(
            "srec",
            &out.join("final.srec"),
            "binary",
            &out.join("final.bin"),
        )?;
    } else {
        std::fs::copy(
            &mut out.join("combined.srec").to_str().unwrap(),
            &mut out.join("final.srec").to_str().unwrap(),
        )?;

        std::fs::copy(
            &mut out.join("combined.elf").to_str().unwrap(),
            &mut out.join("final.elf").to_str().unwrap(),
        )?;

        std::fs::copy(
            &mut out.join("combined.ihex").to_str().unwrap(),
            &mut out.join("final.ihex").to_str().unwrap(),
        )?;

        std::fs::copy(
            &mut out.join("combined.bin").to_str().unwrap(),
            &mut out.join("final.bin").to_str().unwrap(),
        )?;
    }

    let mut gdb_script = File::create(out.join("script.gdb"))?;
    writeln!(
        gdb_script,
        "add-symbol-file {}",
        out.join("kernel").to_slash().unwrap()
    )?;
    for name in toml.tasks.keys() {
        writeln!(
            gdb_script,
            "add-symbol-file {}",
            out.join(name).to_slash().unwrap()
        )?;
    }
    if let Some(bootloader) = toml.bootloader.as_ref() {
        writeln!(
            gdb_script,
            "add-symbol-file {}",
            out.join(&bootloader.name).to_slash().unwrap()
        )?;
    }
    for (path, remap) in &remap_paths {
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
    drop(gdb_script);

    // Bundle everything up into an archive.
    let mut archive =
        Archive::new(out.join(format!("build-{}.zip", toml.name)))?;

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
    archive.copy(cfg, "app.toml")?;
    if let Some(chip) = &toml.chip {
        let chip_file = cfg.parent().unwrap().join(chip);
        let chip_filename =
            chip_file.file_name().unwrap().to_str().unwrap().to_owned();
        archive.copy(chip_file, chip_filename)?;
    }

    let elf_dir = PathBuf::from("elf");
    let tasks_dir = elf_dir.join("task");
    for name in toml.tasks.keys() {
        archive.copy(out.join(name), tasks_dir.join(name))?;
    }
    archive.copy(out.join("kernel"), elf_dir.join("kernel"))?;

    let info_dir = PathBuf::from("info");
    archive.copy(
        out.join("allocations.txt"),
        info_dir.join("allocations.txt"),
    )?;
    archive.copy(out.join("map.txt"), info_dir.join("map.txt"))?;

    let img_dir = PathBuf::from("img");
    archive.copy(out.join("combined.srec"), img_dir.join("combined.srec"))?;
    archive.copy(out.join("combined.elf"), img_dir.join("combined.elf"))?;
    archive.copy(out.join("combined.ihex"), img_dir.join("combined.ihex"))?;
    archive.copy(out.join("combined.bin"), img_dir.join("combined.bin"))?;

    if let Some(bootloader) = toml.bootloader.as_ref() {
        archive
            .copy(out.join(&bootloader.name), img_dir.join(&bootloader.name))?;
    }
    for s in toml.signing.keys() {
        let name = format!("{}_{}.bin", s, toml.signing.get(s).unwrap().method);
        archive.copy(out.join(&name), img_dir.join(&name))?;
    }

    archive.copy(out.join("final.srec"), img_dir.join("final.srec"))?;
    archive.copy(out.join("final.elf"), img_dir.join("final.elf"))?;
    archive.copy(out.join("final.ihex"), img_dir.join("final.ihex"))?;
    archive.copy(out.join("final.bin"), img_dir.join("final.bin"))?;

    //
    // To allow for the image to be flashed based only on the archive (e.g.,
    // by Humility), we pull in our flash configuration, flatten it to pull in
    // any external configuration files, serialize it, and add it to the
    // archive.
    //
    if let Some(mut config) = crate::flash::config(&toml.board.as_str())? {
        config.flatten()?;
        archive.text(img_dir.join("flash.ron"), ron::to_string(&config)?)?;
    }

    archive.finish()?;

    Ok(())
}

/// Prints warning messages about priority inversions
fn check_task_priorities(toml: &Config) -> Result<()> {
    let color_choice = if atty::is(atty::Stream::Stderr) {
        termcolor::ColorChoice::Auto
    } else {
        termcolor::ColorChoice::Never
    };
    let mut out_stream = termcolor::StandardStream::stderr(color_choice);
    let out = &mut out_stream;

    for (name, task) in &toml.tasks {
        for callee in task.task_slots.values() {
            let p = toml
                .tasks
                .get(callee)
                .ok_or_else(|| anyhow!("Invalid task-slot: {}", callee))?
                .priority;
            if p >= task.priority {
                // TODO: once all priority inversions are fixed, return an
                // error so no more can be introduced
                let mut color = ColorSpec::new();
                color.set_fg(Some(Color::Red));
                out.set_color(&color)?;
                write!(out, "Priority inversion: ")?;
                out.reset()?;
                writeln!(
                    out,
                    "task {} (priority {}) calls into {} (priority {})",
                    name, task.priority, callee, p
                )?;
            }
        }
    }

    Ok(())
}

fn generate_header(
    in_binary: &PathBuf,
    out_binary: &PathBuf,
    header_start: u32,
    memories: &IndexMap<String, Range<u32>>,
) -> Result<()> {
    use zerocopy::AsBytes;

    let mut bytes = std::fs::read(in_binary)?;
    let image_len = bytes.len();

    let flash = memories.get("flash").unwrap();
    let ram = memories.get("ram").unwrap();

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

fn do_sign_file(
    sign: &Signing,
    out: &PathBuf,
    src_dir: &PathBuf,
    fname: &str,
    header_start: u32,
    memories: &IndexMap<String, Range<u32>>,
) -> Result<()> {
    if sign.method == "crc" {
        crc_image::update_crc(
            &out.join(format!("{}.bin", fname)),
            &out.join(format!("{}_crc.bin", fname)),
        )
    } else if sign.method == "rsa" {
        let priv_key = sign.priv_key.as_ref().unwrap();
        let root_cert = sign.root_cert.as_ref().unwrap();
        signed_image::sign_image(
            false, // TODO add an option to enable DICE
            &out.join(format!("{}.bin", fname)),
            &src_dir.join(&priv_key),
            &src_dir.join(&root_cert),
            &out.join(format!("{}_rsa.bin", fname)),
            &out.join("CMPA.bin"),
        )
    } else if sign.method == "ecc" {
        // Right now we just generate the header
        generate_header(
            &out.join("combined.bin"),
            &out.join("combined_ecc.bin"),
            header_start,
            memories,
        )
    } else {
        eprintln!("Invalid sign method {}", sign.method);
        std::process::exit(1);
    }
}

fn generate_bootloader_linker_script(
    name: &str,
    map: &IndexMap<String, Range<u32>>,
    sections: Option<&IndexMap<String, String>>,
) {
    // Put the linker script somewhere the linker can find it
    let mut linkscr =
        File::create(Path::new(&format!("target/{}", name))).unwrap();

    writeln!(linkscr, "MEMORY\n{{").unwrap();
    for (name, range) in map {
        let start = range.start;
        let end = range.end;
        let name = name.to_ascii_uppercase();
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

    writeln!(linkscr, "IMAGEA = ORIGIN(IMAGEA_FLASH);").unwrap();
}

fn generate_task_linker_script(
    name: &str,
    map: &BTreeMap<String, Range<u32>>,
    sections: Option<&IndexMap<String, String>>,
    stacksize: u32,
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
    name: &str,
    map: &BTreeMap<String, Range<u32>>,
    stacksize: u32,
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

    Ok(())
}

fn build(
    build_config: BuildConfig,
    dest: PathBuf,
    edges: bool,
    remap_paths: &BTreeMap<PathBuf, &str>,
) -> Result<()> {
    println!("building path {}", build_config.crate_path.display());

    let mut cmd = build_config.cmd("rustc");
    cmd.arg("--release");

    // This works because we control the environment in which we're about
    // to invoke cargo, and never modify CARGO_TARGET in that environment.
    let mut cargo_out = Path::new("target").to_path_buf();

    let remap_path_prefix: String = remap_paths
        .iter()
        .map(|r| format!(" --remap-path-prefix={}={}", r.0.display(), r.1))
        .collect();
    cmd.env(
        "RUSTFLAGS",
        &format!(
            "-C link-arg=-Tlink.x \
             -L {} \
             -C link-arg=-z -C link-arg=common-page-size=0x20 \
             -C link-arg=-z -C link-arg=max-page-size=0x20 \
             -C llvm-args=--enable-machine-outliner=never \
             -C overflow-checks=y \
             {}
             ",
            cargo_out.display(),
            remap_path_prefix,
        ),
    );

    if edges {
        let mut tree = build_config.cmd("tree");
        tree.arg("--edges").arg("features").arg("--verbose");
        println!(
            "Path: {}\nRunning cargo {:?}",
            build_config.crate_path.display(),
            tree
        );
        let tree_status = tree
            .status()
            .context(format!("failed to run edge ({:?})", tree))?;
        if !tree_status.success() {
            bail!("tree command failed, see output for details");
        }
    }

    let status = cmd
        .status()
        .context(format!("failed to run rustc ({:?})", cmd))?;

    if !status.success() {
        bail!("command failed, see output for details");
    }

    cargo_out.push(build_config.out_path);

    println!("{} -> {}", cargo_out.display(), dest.display());
    std::fs::copy(&cargo_out, dest)?;

    Ok(())
}

#[derive(Debug, Clone, Default, Hash)]
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
    kernel: &Kernel,
    tasks: &IndexMap<String, Task>,
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

    for (name, task) in tasks {
        for (mem, &amt) in &task.requires {
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

/// Generate the application descriptor table that the kernel uses to find and
/// start tasks.
///
/// The layout of the table is a series of structs from the `abi` crate:
///
/// - One `App` header.
/// - Some number of `RegionDesc` records describing memory regions.
/// - Some number of `TaskDesc` records describing tasks.
/// - Some number of `Interrupt` records routing interrupts to tasks.
fn make_descriptors(
    target: &str,
    tasks: &IndexMap<String, Task>,
    peripherals: &IndexMap<String, Peripheral>,
    supervisor: Option<&Supervisor>,
    task_allocations: &BTreeMap<String, BTreeMap<String, Range<u32>>>,
    stacksize: Option<u32>,
    outputs: &IndexMap<String, Output>,
    entry_points: &HashMap<String, u32>,
    extra_text: &IndexMap<String, Peripheral>,
) -> Result<KernelConfig> {
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

    // Build a set of all peripheral names used by tasks, which we'll use
    // to filter out unused peripherals.
    let used_peripherals = tasks
        .iter()
        .flat_map(|(_name, task)| task.uses.iter())
        .collect::<HashSet<_>>();

    // ARMv6-M and ARMv7-M require that memory regions be a power of two.
    // ARMv8-M does not.
    let power_of_two_required = match target {
        "thumbv8m.main-none-eabihf" => false,
        "thumbv7em-none-eabihf" => true,
        "thumbv6m-none-eabi" => true,
        t => panic!("Unknown mpu requirements for target '{}'", t),
    };

    for (name, p) in peripherals.iter() {
        if power_of_two_required && !p.size.is_power_of_two() {
            panic!("Memory region for peripheral '{}' is required to be a power of two, but has size {}", name, p.size);
        }

        // skip periperhals that aren't in at least one task's `uses`
        if !used_peripherals.contains(name) {
            continue;
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

    for (name, p) in extra_text.iter() {
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
    for (i, (name, task)) in tasks.iter().enumerate() {
        if power_of_two_required && !task.requires["flash"].is_power_of_two() {
            panic!("Flash for task '{}' is required to be a power of two, but has size {}", task.name, task.requires["flash"]);
        }

        if power_of_two_required && !task.requires["ram"].is_power_of_two() {
            panic!("Ram for task '{}' is required to be a power of two, but has size {}", task.name, task.requires["flash"]);
        }

        // Regions are referenced by index into the table we just generated.
        // Each task has up to 8, chosen from its 'requires' and 'uses' keys.
        let mut task_regions = [0; 8];

        if task.uses.len() + task.requires.len() > 8 {
            panic!(
                "task {} uses {} peripherals and {} memories (too many)",
                name,
                task.uses.len(),
                task.requires.len()
            );
        }

        // Generate a RegionDesc for each uniquely allocated memory region
        // referenced by this task, and install them as entries 0..N in the
        // task's region table.
        let allocs = &task_allocations[name];
        for (ri, (output_name, range)) in allocs.iter().enumerate() {
            let out = &outputs[output_name];
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
            if let Some(&peripheral) = peripheral_index.get(&peripheral_name) {
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
            entry_point: entry_points[name],
            initial_stack: task_allocations[name]["ram"].start
                + task.stacksize.or(stacksize).unwrap(),
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
                        irq: abi::InterruptNum(irq_num),
                        owner: abi::InterruptOwner {
                            task: i as u32,
                            notification,
                        },
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
                            peripherals.get(pname).ok_or_else(|| {
                                anyhow!(
                                    "task {} IRQ {} references peripheral {}, \
                                 which does not exist.",
                                    name,
                                    irq_str,
                                    pname,
                                )
                            })?;
                        let irq_num =
                            periph.interrupts.get(iname).ok_or_else(|| {
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
                            irq: abi::InterruptNum(*irq_num),
                            owner: abi::InterruptOwner {
                                task: i as u32,
                                notification,
                            },
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

    Ok(KernelConfig {
        irqs,
        tasks: task_descs,
        regions,
        supervisor_notification: if let Some(supervisor) = supervisor {
            supervisor.notification
        } else {
            // TODO: this exists for back-compat with incredibly early Hubris,
            // we can likely remove it.
            0
        },
    })
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

fn cargo_clean(name: &str, target: &str) -> Result<()> {
    println!("cleaning {}", name);

    let mut cmd = Command::new("cargo");
    cmd.arg("clean");
    cmd.arg("-p");
    cmd.arg(name);
    cmd.arg("--release");
    cmd.arg("--target");
    cmd.arg(target);

    let status = cmd
        .status()
        .context(format!("failed to cargo clean ({:?})", cmd))?;

    if !status.success() {
        bail!("command failed, see output for details");
    }

    Ok(())
}

fn resolve_task_slots(
    task_name: &String,
    all_tasks_toml: &IndexMap<String, Task>,
    task_bin: &PathBuf,
    verbose: bool,
) -> Result<()> {
    use scroll::{Pread, Pwrite};

    let task_toml = &all_tasks_toml[task_name];

    let in_task_bin = std::fs::read(task_bin)?;
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
            match all_tasks_toml.get_index_of(target_task_name) {
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

        if verbose {
            println!(
                "Task '{}' task_slot '{}' changed from task index {:#x} to task index {:#x}",
                task_name, entry.slot_name, in_task_idx, target_task_idx
            );
        }
    }

    Ok(std::fs::write(task_bin, out_task_bin)?)
}
