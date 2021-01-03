use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::hash::Hasher;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use indexmap::IndexMap;
use path_slash::PathBufExt;

use crate::{Config, LoadSegment, Output, Peripheral, Supervisor, Task};

use lpc55_support::{crc_image, signed_image};

pub fn package(verbose: bool, cfg: &Path) -> Result<()> {
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

    // Allocate space for the kernel.
    let kern_memory = allocate(&mut memories, &toml.kernel.requires)?;
    println!("kernel: {:x?}", kern_memory);

    // Allocate space for tasks.
    let mut task_memory = IndexMap::new();
    for (name, task) in &toml.tasks {
        let mem = allocate(&mut memories, &task.requires)?;
        task_memory.insert(name.clone(), mem);
    }

    let mut infofile = File::create(out.join("allocations.txt"))?;
    writeln!(infofile, "kernel: {:#x?}", kern_memory)?;
    writeln!(infofile, "tasks: {:#x?}", task_memory)?;
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

    for name in toml.tasks.keys() {
        let task_toml = &toml.tasks[name];
        generate_task_linker_script(
            "memory.x",
            &task_memory[name],
            Some(&task_toml.sections),
        );

        fs::copy("task-link.x", "target/link.x")?;

        build(
            &toml.target,
            &toml.board,
            &src_dir.join(&task_toml.path),
            &task_toml.name,
            &task_toml.features,
            out.join(name),
            verbose,
            &task_names,
            &toml.secure,
        )?;

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

    // Format the descriptors for the kernel build.
    let mut descriptor_text = vec![];
    for word in make_descriptors(
        &toml.target,
        &toml.tasks,
        &toml.peripherals,
        toml.supervisor.as_ref(),
        &task_memory,
        &toml.outputs,
        &entry_points,
    )? {
        descriptor_text.push(format!("LONG(0x{:08x});", word));
    }
    let descriptor_text = descriptor_text.join("\n");

    generate_kernel_linker_script("memory.x", &kern_memory, &descriptor_text);

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
        "",
        &toml.secure,
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

    if let Some(signing) = toml.sign_method.as_ref() {
        if signing.method == "crc" {
            crc_image::update_crc(
                &out.join("combined.bin"),
                &out.join("combined_crc.bin"),
            )?;
        } else if signing.method == "secure_boot" {
            let priv_key = signing.priv_key.as_ref().unwrap();
            let root_cert = signing.root_cert.as_ref().unwrap();
            signed_image::sign_image(
                &out.join("combined.bin"),
                &src_dir.join(&priv_key),
                &src_dir.join(&root_cert),
                &out.join("combined_signed.bin"),
                &out.join("CMPA.bin"),
            )?;
        } else {
            eprintln!("Invalid sign method {}", signing.method);
            std::process::exit(1);
        }
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
    if let Some(signing) = toml.sign_method.as_ref() {
        if signing.method == "crc" {
            archive.copy(
                out.join("combined_crc.bin"),
                img_dir.join("combined_crc.bin"),
            )?;
        } else if signing.method == "secure_boot" {
            archive.copy(
                out.join("combined_signed.bin"),
                img_dir.join("combined_signed.bin"),
            )?;
            archive.copy(out.join("CMPA.bin"), img_dir.join("CMPA.bin"))?;
        }
    }

    archive.finish()?;

    Ok(())
}

fn generate_task_linker_script(
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
            "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
            name,
            start,
            end - start
        )
        .unwrap();
    }
    write!(linkscr, "}}").unwrap();

    // The task may have defined additional section-to-memory mappings.
    if let Some(map) = sections {
        writeln!(linkscr, "SECTIONS {{").unwrap();
        for (section, memory) in map {
            writeln!(linkscr, "  .{} (NOLOAD) : ALIGN(4) {{", section).unwrap();
            writeln!(linkscr, "    *(.{} .{}.*);", section, section).unwrap();
            writeln!(linkscr, "  }} > {}", memory.to_ascii_uppercase())
                .unwrap();
        }
        writeln!(linkscr, "}} INSERT BEFORE .got").unwrap();
    }
}

fn generate_kernel_linker_script(
    name: &str,
    map: &IndexMap<String, Range<u32>>,
    descriptor: &str,
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
    writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
    writeln!(linkscr, "SECTIONS {{").unwrap();
    writeln!(linkscr, "  .hubris_app_table : AT(__erodata) {{").unwrap();
    writeln!(linkscr, "    hubris_app_table = .;").unwrap();
    writeln!(linkscr, "{}", descriptor).unwrap();
    writeln!(linkscr, "  }} > FLASH").unwrap();
    writeln!(linkscr, "}} INSERT AFTER .data").unwrap();
}

fn build(
    target: &str,
    board_name: &str,
    path: &Path,
    name: &str,
    features: &[String],
    dest: PathBuf,
    verbose: bool,
    task_names: &str,
    secure: &Option<bool>,
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
        -C link-arg=-z -C link-arg=max-page-size=0x20",
            canonical_cargo_out_dir.display()
        ),
    );

    cmd.env("HUBRIS_TASKS", task_names);
    cmd.env("HUBRIS_BOARD", board_name);

    if let Some(s) = secure {
        if *s {
            cmd.env("HUBRIS_SECURE", "1");
        } else {
            cmd.env("HUBRIS_SECURE", "0");
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

fn allocate(
    free: &mut IndexMap<String, Range<u32>>,
    needs: &IndexMap<String, u32>,
) -> Result<IndexMap<String, Range<u32>>> {
    let mut taken = IndexMap::new();
    for (name, need) in needs {
        let need = if need.is_power_of_two() {
            *need
        } else {
            need.next_power_of_two()
        };
        let need_mask = need - 1;

        if let Some(range) = free.get_mut(name) {
            let base = (range.start + need_mask) & !need_mask;
            if base >= range.end || need > range.end - base {
                bail!(
                    "out of {}: can't allocate {} more after base {:x}",
                    name,
                    need,
                    base
                )
            }
            let end = base + need;
            taken.insert(name.clone(), base..end);
            range.start = end;
        } else {
            bail!("unknown output memory {}", name);
        }
    }
    Ok(taken)
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
    task_allocations: &IndexMap<String, IndexMap<String, Range<u32>>>,
    outputs: &IndexMap<String, Output>,
    entry_points: &HashMap<String, u32>,
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
        for (j, name) in task.uses.iter().enumerate() {
            task_regions[allocs.len() + j] = peripheral_index[name] as u8;
        }

        let mut flags = abi::TaskFlags::empty();
        if task.start {
            flags |= abi::TaskFlags::START_AT_BOOT;
        }
        task_descs.push(abi::TaskDesc {
            regions: task_regions,
            entry_point: entry_points[name],
            initial_stack: task_allocations[name]["ram"].end,
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
            if (notification & (notification - 1)) != 0 {
                bail!(
                    "task {}: IRQ {}: notification mask (0b{:b}) \
                    has multiple bits set",
                    name,
                    irq_str,
                    notification
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
