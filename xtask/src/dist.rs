use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use indexmap::IndexMap;
use path_slash::PathBufExt;

use crate::{Config, LoadSegment, Output, Peripheral, Supervisor, Task};

pub fn package(verbose: bool, cfg: &Path) -> Result<(), Box<dyn Error>> {
    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;
    drop(cfg_contents);

    let mut out = PathBuf::from("target");
    out.push(&toml.name);
    out.push("dist");

    std::fs::create_dir_all(&out)?;

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
    for name in toml.tasks.keys() {
        let task_toml = &toml.tasks[name];
        build(
            &toml.target,
            &toml.board,
            &src_dir.join(&task_toml.path),
            &task_toml.name,
            &task_toml.features,
            &task_memory[name],
            out.join(name),
            verbose,
            &[("HUBRIS_TASKS", &task_names), ("HUBRIS_TASK_SELF", name)],
        )?;
        let ep = load_elf(&out.join(name), &mut all_output_sections)?;
        entry_points.insert(name.clone(), ep);
    }

    // Format the descriptors for the kernel build.
    let mut descriptor_text = vec![];
    for word in make_descriptors(
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

    // Build the kernel.
    build(
        &toml.target,
        &toml.board,
        &src_dir.join(&toml.kernel.path),
        &toml.kernel.name,
        &toml.kernel.features,
        &kern_memory,
        out.join("kernel"),
        verbose,
        &[("HUBRIS_DESCRIPTOR", &descriptor_text)],
    )?;
    let kentry = load_elf(&out.join("kernel"), &mut all_output_sections)?;

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

    // Write out combined SREC file.
    let mut srec_out = vec![];
    srec_out.push(srec::Record::S0("hubris".to_string()));
    for (base, sec) in all_output_sections {
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
    std::fs::write(out.join("combined.srec"), srec_image)?;

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

    println!("doing objcopy");

    let srec_path = out.join("combined.srec");
    let elf_path = out.join("combined.elf");

    let mut cmd = Command::new("arm-none-eabi-objcopy");
    cmd.arg("-Isrec")
        .arg("-O")
        .arg("elf32-littlearm")
        .arg(srec_path)
        .arg(elf_path);

    let status = cmd.status()?;
    if !status.success() {
        return Err("command failed, see output for details".into());
    }

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

    archive.finish()?;

    Ok(())
}

fn build(
    target: &str,
    board_name: &str,
    path: &Path,
    name: &str,
    features: &[String],
    alloc: &IndexMap<String, Range<u32>>,
    dest: PathBuf,
    verbose: bool,
    meta: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
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
    cmd.arg("build")
        .arg("--release")
        .arg("--no-default-features")
        .arg("--target")
        .arg(target)
        .arg("--bin")
        .arg(name);
    if verbose {
        cmd.arg("-v");
    }
    if !features.is_empty() {
        cmd.arg("--features");
        cmd.arg(features.join(","));
    }

    cmd.current_dir(path);
    cmd.env(
        "RUSTFLAGS",
        "-C link-arg=-Tlink.x \
                          -C link-arg=-z -C link-arg=common-page-size=0x20 \
                          -C link-arg=-z -C link-arg=max-page-size=0x20",
    );
    cmd.env("HUBRIS_PKG_MAP", serde_json::to_string(&alloc)?);
    for (key, val) in meta {
        cmd.env(key, val);
    }
    cmd.env("HUBRIS_BOARD", board_name);

    let status = cmd.status()?;
    if !status.success() {
        return Err("command failed, see output for details".into());
    }

    let mut cargo_out = cargo_output_dir(target, path)?;
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
) -> Result<IndexMap<String, Range<u32>>, Box<dyn Error>> {
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
                return Err(format!(
                    "out of {}: can't allocate {} more after base {:x}",
                    name, need, base
                )
                .into());
            }
            let end = base + need;
            taken.insert(name.clone(), base..end);
            range.start = end;
        } else {
            return Err(format!("unknown output memory {}", name).into());
        }
    }
    Ok(taken)
}

fn cargo_output_dir(
    target: &str,
    path: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
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
        return Err("command failed, see output for details".into());
    }

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(PathBuf::from(meta["target_directory"].as_str().unwrap()))
}

fn make_descriptors(
    tasks: &IndexMap<String, Task>,
    peripherals: &IndexMap<String, Peripheral>,
    supervisor: Option<&Supervisor>,
    task_allocations: &IndexMap<String, IndexMap<String, Range<u32>>>,
    outputs: &IndexMap<String, Output>,
    entry_points: &HashMap<String, u32>,
) -> Result<Vec<u32>, Box<dyn Error>> {
    let mut words = vec![];

    let region_count = 1 + tasks.len() * 2 + peripherals.len();

    let mut peripheral_index = IndexMap::new();
    for (i, name) in peripherals.keys().enumerate() {
        peripheral_index.insert(name.clone(), 1 + i + tasks.len() * 2);
    }

    let irq_count = tasks.values().map(|t| t.interrupts.len()).sum::<usize>();

    // App header
    words.push(0x1DE_fa7a1);
    words.push(tasks.len() as u32);
    words.push(region_count as u32);
    words.push(irq_count as u32);
    if let Some(supervisor) = supervisor {
        words.push(supervisor.notification);
    }
    // pad out to 32 bytes
    words.resize(32 / 4, 0);

    // Task descriptors
    for (i, (name, task)) in tasks.iter().enumerate() {
        let mut regions = [0; 8];
        regions[0] = (1 + 2 * i) as u8;
        regions[1] = (1 + 2 * i + 1) as u8;

        if task.uses.len() > 6 {
            panic!("too many peripherals used by task {}", name);
        }

        for (j, name) in task.uses.iter().enumerate() {
            regions[2 + j] = peripheral_index[name] as u8;
        }

        // Region table indices
        words.push(
            u32::from(regions[0])
                | u32::from(regions[1]) << 8
                | u32::from(regions[2]) << 16
                | u32::from(regions[3]) << 24,
        );
        words.push(
            u32::from(regions[4])
                | u32::from(regions[5]) << 8
                | u32::from(regions[6]) << 16
                | u32::from(regions[7]) << 24,
        );

        // Entry point
        words.push(entry_points[name]);
        // Initial stack
        words.push(task_allocations[name]["ram"].end);
        // Priority
        words.push(task.priority);
        // Flags
        let flags = if task.start { 1 } else { 0 };
        words.push(flags);
    }

    // Region descriptors

    // Null region
    words.push(0);
    words.push(32);
    words.push(0); // no rights
    words.push(0);

    // Task regions
    for alloc in task_allocations.values() {
        for (output_name, range) in alloc {
            let out = &outputs[output_name];
            let atts = u32::from(out.read)
                | u32::from(out.write) << 1
                | u32::from(out.execute) << 2
                // no option for setting DEVICE for this region
                ;

            words.push(range.start);
            words.push(range.end - range.start);
            words.push(atts);
            words.push(0);
        }
    }

    // Peripheral regions
    for p in peripherals.values() {
        // Peripherals are always mapped as Device + Read + Write.
        let atts = 0b1011;

        words.push(p.address);
        words.push(p.size);
        words.push(atts);
        words.push(0);
    }

    // Interrupt response records.
    for (i, task) in tasks.values().enumerate() {
        for (irq_str, &notmask) in &task.interrupts {
            let irq_num = irq_str.parse::<u32>()?;
            words.push(irq_num);
            words.push(i as u32);
            words.push(notmask);
        }
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
) -> Result<u32, Box<dyn Error>> {
    let srec_text = std::fs::read_to_string(input)?;
    for record in srec::reader::read_records(&srec_text) {
        let record = record?;
        match record {
            srec::Record::S3(data) => {
                // Check for address overlap
                let range =
                    data.address.0..data.address.0 + data.data.len() as u32;
                if let Some(overlap) = output.range(range.clone()).next() {
                    return Err(format!(
                        "{}: record address range {:x?} overlaps {:x}",
                        input.display(),
                        range,
                        overlap.0
                    )
                    .into());
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
) -> Result<u32, Box<dyn Error>> {
    use goblin::container::Container;
    use goblin::elf::program_header::PT_LOAD;

    let file_image = std::fs::read(input)?;
    let elf = goblin::elf::Elf::parse(&file_image)?;

    if elf.header.container()? != Container::Little {
        return Err("where did you get a big-endian image?".into());
    }
    if elf.header.e_machine != goblin::elf::header::EM_ARM {
        return Err("this is not an ARM file".into());
    }

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

        // Check for address overlap
        let range = addr..addr + size as u32;
        if let Some(overlap) = output.range(range.clone()).next() {
            return Err(format!(
                "{}: record address range {:x?} overlaps {:x}",
                input.display(),
                range,
                overlap.0
            )
            .into());
        }
        output.insert(
            addr,
            LoadSegment {
                source_file: input.into(),
                data: file_image[offset..offset + size].to_vec(),
            },
        );
    }
    Ok(elf.header.e_entry as u32)
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
    fn new(dest: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
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
    ) -> Result<(), Box<dyn Error>> {
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
    ) -> Result<(), Box<dyn Error>> {
        self.inner
            .start_file_from_path(zip_path.as_ref(), self.opts)?;
        self.inner.write_all(contents.as_ref().as_bytes())?;
        Ok(())
    }

    /// Completes the archive and moves it to its intended location.
    ///
    /// If you drop an `Archive` without calling this, it will leave a temporary
    /// file rather than creating the final archive.
    fn finish(self) -> Result<(), Box<dyn Error>> {
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
fn get_git_status() -> Result<(String, bool), Box<dyn Error>> {
    let mut cmd = Command::new("git");
    cmd.arg("rev-parse").arg("HEAD");
    let out = cmd.output()?;
    if !out.status.success() {
        return Err("git rev-parse failed".into());
    }
    let rev = std::str::from_utf8(&out.stdout)?.trim().to_string();

    let mut cmd = Command::new("git");
    cmd.arg("diff-index").arg("--quiet").arg("HEAD").arg("--");
    let status = cmd.status()?;

    Ok((rev, !status.success()))
}
