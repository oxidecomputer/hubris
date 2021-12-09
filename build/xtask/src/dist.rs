// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File};
use std::hash::Hasher;
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use indexmap::IndexMap;
use path_slash::PathBufExt;

use crate::{
    elf, task_slot, Config, LoadSegment, Output, Peripheral, Signing,
    Supervisor, Task,
};

use lpc55_sign::{crc_image, sign_ecc, signed_image};

/// In practice, applications with active interrupt activity tend to use about
/// 650 bytes of stack. Because kernel stack overflows are annoying, we've
/// padded that a bit.
const DEFAULT_KERNEL_STACK: u32 = 1024;

pub fn package(
    verbose: bool,
    edges: bool,
    cfg: &Path,
    tasks_to_build: Option<Vec<String>>,
) -> Result<()> {
    // If we're using filters, we change behavior at the end. Record this in a
    // convenient flag.
    let partial_build = tasks_to_build.is_some();

    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut hasher = DefaultHasher::new();
    hasher.write(&cfg_contents);
    let buildhash = hasher.finish();
    drop(cfg_contents);

    let mut out = PathBuf::from("target");
    let buildstamp_file = out.join("buildstamp");

    out.push(&toml.name);
    out.push("dist");

    std::fs::create_dir_all(&out)?;

    let rebuild = match std::fs::read(&buildstamp_file) {
        Ok(contents) => {
            if let Ok(contents) = std::str::from_utf8(&contents) {
                if let Ok(cmp) = u64::from_str_radix(contents, 16) {
                    buildhash != cmp
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
        println!("{} = {:x?}", name, range);
    }
    let starting_memories = memories.clone();

    // Allocate memories.
    let allocs = allocate_all(&toml.kernel, &toml.tasks, &mut memories)?;

    println!("Used:");
    for (name, new_range) in &memories {
        let orig_range = &starting_memories[name];
        println!("{}: 0x{:x}", name, new_range.start - orig_range.start);
    }

    let mut infofile = File::create(out.join("allocations.txt"))?;
    writeln!(infofile, "kernel: {:#x?}", allocs.kernel)?;
    writeln!(infofile, "tasks: {:#x?}", allocs.tasks)?;
    drop(infofile);

    // Build each task.
    let task_names = toml.tasks.keys().cloned().collect::<Vec<_>>();
    let task_names = task_names.join(",");
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
    std::fs::write(&buildstamp_file, format!("{:x}", buildhash))?;
    let mut shared_syms: Option<&[String]> = None;

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

        let kernel_start = allocs.kernel.get("flash").unwrap().start;

        if kernel_start != bootloader_memory.get("FLASH").unwrap().end {
            panic!("mismatch between bootloader end and hubris start! check app.toml!");
        }

        shared_syms = Some(&bootloader.sharedsyms);

        generate_bootloader_linker_script(
            "memory.x",
            &bootloader_memory,
            Some(&bootloader.sections),
            &bootloader.sharedsyms,
        );

        // If there is a stray link.x around from a previous build remove it
        // The file not existing isn't an error
        let _ = fs::remove_file("target/link.x");

        build(
            &toml.target,
            &toml.board,
            &src_dir.join(&bootloader.path),
            &bootloader.name,
            &bootloader.features,
            out.join(&bootloader.name),
            verbose,
            edges,
            &task_names,
            &None,
            &shared_syms,
            &None,
            &toml.config,
        )?;

        // Need a bootloader binary for signing
        objcopy_translate_format(
            "elf32-littlearm",
            &out.join(&bootloader.name),
            "binary",
            &out.join("bootloader.bin"),
        )?;

        if let Some(signing) = toml.signing.get("bootloader") {
            do_sign_file(signing, &out, &src_dir, "bootloader")?;
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
            writeln!(linkscr, "BYTE(0x{:x})", b).unwrap();
        }

        drop(linkscr);
    } else {
        // Just create a new empty file
        File::create(Path::new(&format!("target/table.ld"))).unwrap();
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

        build(
            &toml.target,
            &toml.board,
            &src_dir.join(&task_toml.path),
            &task_toml.name,
            &task_toml.features,
            out.join(name),
            verbose,
            edges,
            &task_names,
            &toml.secure,
            &shared_syms,
            &task_toml.config,
            &toml.config,
        )
        .context(format!("failed to build {}", name))?;

        resolve_task_slots(name, &toml.tasks, &out.join(name), verbose)?;

        let (ep, flash) = load_elf(&out.join(name), &mut all_output_sections)?;

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

    // Format the descriptors for the kernel build.
    let mut descriptor_text = vec![];
    for word in make_descriptors(
        &toml.target,
        &toml.tasks,
        &toml.peripherals,
        toml.supervisor.as_ref(),
        &allocs.tasks,
        toml.stacksize,
        &toml.outputs,
        &entry_points,
        &toml.extratext,
    )? {
        descriptor_text.push(format!("LONG(0x{:08x});", word));
    }
    let descriptor_text = descriptor_text.join("\n");

    generate_kernel_linker_script(
        "memory.x",
        &allocs.kernel,
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &descriptor_text,
    )?;

    // this one was for the tasks, but we don't want to use it for the kernel
    fs::remove_file("target/link.x")?;

    // Build the kernel.
    build(
        &toml.target,
        &toml.board,
        &src_dir.join(&toml.kernel.path),
        &toml.kernel.name,
        &toml.kernel.features,
        out.join("kernel"),
        verbose,
        edges,
        "",
        &toml.secure,
        &None,
        &None,
        &toml.config,
    )?;
    let (kentry, _) = load_elf(&out.join("kernel"), &mut all_output_sections)?;

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
        do_sign_file(signing, &out, &src_dir, "combined")?;
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

    archive.finish()?;

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
        let priv_key = sign.priv_key.as_ref().unwrap();
        sign_ecc::ecc_sign_image(
            &out.join(format!("{}.bin", fname)),
            &src_dir.join(&priv_key),
            &out.join(format!("{}_ecc.bin", fname)),
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
    sharedsyms: &[String],
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
        writeln!(linkscr, "}} INSERT BEFORE .got")?;
    }

    Ok(())
}

fn generate_kernel_linker_script(
    name: &str,
    map: &BTreeMap<String, Range<u32>>,
    stacksize: u32,
    descriptor: &str,
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
    writeln!(linkscr, "SECTIONS {{").unwrap();
    writeln!(linkscr, "  .hubris_app_table : AT(__erodata) {{").unwrap();
    writeln!(linkscr, "    hubris_app_table = .;").unwrap();
    writeln!(linkscr, "{}", descriptor).unwrap();
    writeln!(linkscr, "  }} > FLASH").unwrap();
    writeln!(linkscr, "}} INSERT AFTER .data").unwrap();

    Ok(())
}

fn build(
    target: &str,
    board_name: &str,
    path: &Path,
    name: &str,
    features: &[String],
    dest: PathBuf,
    verbose: bool,
    edges: bool,
    task_names: &str,
    secure: &Option<bool>,
    shared_syms: &Option<&[String]>,
    config: &Option<toml::Value>,
    app_config: &Option<toml::Value>,
) -> Result<()> {
    println!("building path {}", path.display());

    // NOTE: current_dir's docs suggest that you should use canonicalize for
    // portability. However, that's for when you're doing stuff like:
    //
    // Command::new("../cargo")
    //
    // That is, when you have a relative path to the binary being executed. We
    // are not including a path in the binary name, so everything is peachy. If
    // you change this line below, make sure to canonicalize path.
    let mut cmd = Command::new("cargo");
    cmd.arg("rustc")
        .arg("--release")
        .arg("--no-default-features")
        .arg("--target")
        .arg(target);

    if verbose {
        cmd.arg("-v");
    }
    if !features.is_empty() {
        cmd.arg("--features");
        cmd.arg(features.join(","));
    }

    let mut cargo_out = cargo_output_dir(target, path)?;

    // the target dir for each project is set to the same dir, but it ends up
    // looking something like:
    //
    // foo/bar/../target
    // foo/baz/../target
    //
    // these resolve to the same dirs but since their text changes, this causes
    // rebuilds. by canonicalizing it, you get foo/target for every one.
    let canonical_cargo_out_dir = fs::canonicalize(&cargo_out)?;

    cmd.current_dir(path);
    cmd.env(
        "RUSTFLAGS",
        &format!(
            "-C link-arg=-Tlink.x \
             -L {} \
             -C link-arg=-z -C link-arg=common-page-size=0x20 \
             -C link-arg=-z -C link-arg=max-page-size=0x20 \
             -C llvm-args=--enable-machine-outliner=never \
             -C overflow-checks=y",
            canonical_cargo_out_dir.display()
        ),
    );

    cmd.env("HUBRIS_TASKS", task_names);
    cmd.env("HUBRIS_BOARD", board_name);

    if let Some(s) = shared_syms {
        if !s.is_empty() {
            cmd.env("SHARED_SYMS", s.join(","));
        }
    }

    if let Some(s) = secure {
        if *s {
            cmd.env("HUBRIS_SECURE", "1");
        } else {
            cmd.env("HUBRIS_SECURE", "0");
        }
    }

    //
    // We allow for task- and app-specific configuration to be passed
    // via environment variables to build.rs scripts that may choose to
    // incorporate configuration into compilation.
    //
    if let Some(config) = config {
        let env = toml::to_string(&config).unwrap();
        cmd.env("HUBRIS_TASK_CONFIG", env);
    }

    if let Some(app_config) = app_config {
        let env = toml::to_string(&app_config).unwrap();
        cmd.env("HUBRIS_APP_CONFIG", env);
    }

    if edges {
        let mut tree = Command::new("cargo");
        tree.arg("tree")
            .arg("--no-default-features")
            .arg("--edges")
            .arg("features")
            .arg("--verbose");
        if !features.is_empty() {
            tree.arg("--features");
            tree.arg(features.join(","));
        }
        tree.current_dir(path);
        println!("Path: {}\nRunning cargo {:?}", path.display(), tree);
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

    cargo_out.push(target);
    cargo_out.push("release");
    cargo_out.push(name);

    println!("{} -> {}", cargo_out.display(), dest.display());
    std::fs::copy(&cargo_out, dest)?;

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
    tasks: &IndexMap<String, crate::Task>,
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
    for (name, &amt) in kernel_requests {
        if !amt.is_power_of_two() {
            bail!("kernel, memory region {}: requirement {} is not a power of two.",
                name, amt);
        }
    }

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
                allocs.kernel.insert(
                    region.to_string(),
                    allocate_one(region, sz, avail)?,
                );
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

fn cargo_output_dir(target: &str, path: &Path) -> Result<PathBuf> {
    // NOTE: current_dir's docs suggest that you should use canonicalize for
    // portability. However, that's for when you're doing stuff like:
    //
    // Command::new("../cargo")
    //
    // That is, when you have a relative path to the binary being executed. We
    // are not including a path in the binary name, so everything is peachy. If
    // you change this line below, make sure to canonicalize path.
    let mut cmd = Command::new("cargo");
    cmd.arg("metadata").arg("--filter-platform").arg(target);
    cmd.current_dir(path);

    let output = cmd.output()?;
    if !output.status.success() {
        bail!("command failed, see output for details");
    }

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(PathBuf::from(meta["target_directory"].as_str().unwrap()))
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
) -> Result<Vec<u32>> {
    // Generate the three record sections concurrently, using three separate
    // vecs that we'll later concatenate.
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
    let power_of_two_required = match target {
        "thumbv8m.main-none-eabihf" => false,
        "thumbv7em-none-eabihf" => true,
        t => panic!("Unknown mpu requirements for target '{}'", t),
    };

    for (name, p) in peripherals.iter() {
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
                + task.stacksize.unwrap_or(stacksize.unwrap()),
            priority: task.priority,
            flags,
        });

        // Interrupts.
        for (irq_str, &notification) in &task.interrupts {
            let irq_num = irq_str.parse::<u32>()?;

            // While it's possible to conceive of a world in which one
            // might want to have a single interrupt set multiple notification
            // bits, it's much easier to conceive of a world in which one
            // has misunderstood that the second number in the interrupt
            // tuple is in fact a mask, not an index.
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
    }

    // Assemble everything into the final image.
    let mut words = vec![];

    // App header
    words.push(0x1DE_fa7a1);
    words.push(task_descs.len() as u32);
    words.push(regions.len() as u32);
    words.push(irqs.len() as u32);
    if let Some(supervisor) = supervisor {
        words.push(supervisor.notification);
    }
    // pad out to 32 bytes
    words.resize(32 / 4, 0);

    // Flatten region descriptors
    for rdesc in regions {
        words.push(rdesc.base);
        words.push(rdesc.size);
        words.push(rdesc.attributes.bits());
        words.push(rdesc.reserved_zero);
    }

    // Flatten task descriptors
    for tdesc in task_descs {
        // Region table indices
        words.push(
            u32::from(tdesc.regions[0])
                | u32::from(tdesc.regions[1]) << 8
                | u32::from(tdesc.regions[2]) << 16
                | u32::from(tdesc.regions[3]) << 24,
        );
        words.push(
            u32::from(tdesc.regions[4])
                | u32::from(tdesc.regions[5]) << 8
                | u32::from(tdesc.regions[6]) << 16
                | u32::from(tdesc.regions[7]) << 24,
        );

        words.push(tdesc.entry_point);
        words.push(tdesc.initial_stack);
        words.push(tdesc.priority);
        words.push(tdesc.flags.bits());
    }

    // Flatten interrupt response records.
    for idesc in irqs {
        words.push(idesc.irq);
        words.push(idesc.task);
        words.push(idesc.notification);
    }

    Ok(words)
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
            bail!(
                "{}: record address range {:x?} overlaps {:x}",
                input.display(),
                range,
                overlap.0
            );
        }
        output.insert(
            addr,
            LoadSegment {
                source_file: input.into(),
                data: file_image[offset..offset + size].to_vec(),
            },
        );
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
                "Task '{}' task_slot '{}' changed from task index 0x{:x} to task index 0x{:x}",
                task_name, entry.slot_name, in_task_idx, target_task_idx
            );
        }
    }

    Ok(std::fs::write(task_bin, out_task_bin)?)
}
