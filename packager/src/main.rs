use std::path::{PathBuf, Path};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::ops::Range;
use std::io::Write;

use serde::Deserialize;
use structopt::StructOpt;
use indexmap::IndexMap;

/// Builds a collection of cross-compiled binaries at non-overlapping addresses,
/// and then combines them into a system image with an application descriptor.
#[derive(Clone, Debug, StructOpt)]
#[structopt(max_term_width = 80)]
struct Args {
    /// Path to the image configuration file, in TOML.
    cfg: PathBuf,
    /// Path to the output directory, where this tool will place a set of ELF
    /// files, a combined SREC file, and a text file documenting the memory
    /// layout.
    out: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    name: String,
    target: String,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Kernel {
    path: PathBuf,
    name: String,
    requires: IndexMap<String, u32>,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Supervisor {
    notification: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Output {
    address: u32,
    size: u32,
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write: bool,
    #[serde(default)]
    execute: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Task {
    path: PathBuf,
    name: String,
    requires: IndexMap<String, u32>,
    priority: u32,
    #[serde(default)]
    uses: Vec<String>,
    #[serde(default)]
    start: bool,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    interrupts: IndexMap<String, u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Peripheral {
    address: u32,
    size: u32,
}

struct LoadSegment {
    source_file: PathBuf,
    data: Vec<u8>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::from_args();
    let cfg = std::fs::read(&args.cfg)?;
    let toml: Config = toml::from_slice(&cfg)?;
    drop(cfg);

    let mut src_dir = args.cfg.clone();
    src_dir.pop();

    let mut memories = IndexMap::new();
    for (name, out) in &toml.outputs {
        if let Some(end) = out.address.checked_add(out.size) {
            memories.insert(name.clone(), out.address..end);
        } else {
            eprintln!("output {}: address {:08x} size {:x} would overflow",
                name, out.address, out.size);
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

    let mut infofile = std::fs::File::create(args.out.join("allocations.txt"))?;
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
        build(&toml.target, &src_dir.join(&task_toml.path), &task_toml.name, &task_toml.features, &task_memory[name], args.out.join(name),
            &[("HUBRIS_TASKS", &task_names), ("HUBRIS_TASK_SELF", name)])?;
        let ep = load_elf(&args.out.join(name), &mut all_output_sections)?;
        entry_points.insert(name.clone(), ep);
    }

    // Format the descriptors for the kernel build.
    let mut descriptor_text = vec![];
    for word in make_descriptors(&toml.tasks, &toml.peripherals, toml.supervisor.as_ref(), &task_memory, &entry_points)? {
        descriptor_text.push(format!("LONG(0x{:08x});", word));
    }
    let descriptor_text = descriptor_text.join("\n");

    // Build the kernel.
    build(&toml.target, &src_dir.join(&toml.kernel.path), &toml.kernel.name, &toml.kernel.features, &kern_memory, args.out.join("kernel"),
        &[("HUBRIS_DESCRIPTOR", &descriptor_text)])?;
    let kentry = load_elf(&args.out.join("kernel"), &mut all_output_sections)?;

    // Write a map file, because that seems nice.
    let mut mapfile = std::fs::File::create(&args.out.join("map.txt"))?;
    writeln!(mapfile, "ADDRESS  END          SIZE FILE")?;
    for (base, sec) in &all_output_sections {
        let size = sec.data.len() as u32;
        let end = base + size;
        writeln!(mapfile, "{:08x} {:08x} {:>8x} {}",
            base, end, size, sec.source_file.display())?;
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
    std::fs::write(args.out.join("combined.srec"), srec_image)?;

    let mut gdb_script = std::fs::File::create(args.out.join("script.gdb"))?;
    writeln!(gdb_script, "add-symbol-file {}", args.out.join("kernel").display())?;
    for name in toml.tasks.keys() {
        writeln!(gdb_script, "add-symbol-file {}", args.out.join(name).display())?;
    }
    drop(gdb_script);

    Ok(())
}

fn build(
    target: &str,
    path: &Path,
    name: &str,
    features: &[String],
    alloc: &IndexMap<String, Range<u32>>,
    dest: PathBuf,
    meta: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
    use std::process::Command;

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
    if !features.is_empty() {
        cmd.arg("--features");
        for feature in features {
            cmd.arg(feature);
        }
    }

    cmd.current_dir(path);
    cmd.env("RUSTFLAGS", "-C link-arg=-Tlink.x");
    cmd.env("HUBRIS_PKG_MAP", serde_json::to_string(&alloc)?);
    for (key, val) in meta {
        cmd.env(key, val);
    }

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

fn allocate(free: &mut IndexMap<String, Range<u32>>, needs: &IndexMap<String, u32>) -> Result<IndexMap<String, Range<u32>>, Box<dyn Error>> {
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
                return Err(format!("out of {}: can't allocate {} more after base {:x}",
                        name, need, base).into());
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

fn cargo_output_dir(target: &str, path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    use std::process::Command;

    // NOTE: current_dir's docs suggest that you should use canonicalize for
    // portability. However, that's for when you're doing stuff like:
    //
    // Command::new("../cargo")
    //
    // That is, when you have a relative path to the binary being executed. We
    // are not including a path in the binary name, so everything is peachy. If
    // you change this line below, make sure to canonicalize path.
    let mut cmd = Command::new("cargo");
    cmd.arg("metadata")
        .arg("--filter-platform")
        .arg(target);
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
    words.resize(32/4, 0);

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
            | u32::from(regions[3]) << 24);
        words.push(
            u32::from(regions[4])
            | u32::from(regions[5]) << 8
            | u32::from(regions[6]) << 16
            | u32::from(regions[7]) << 24);

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
        for range in alloc.values() {
            words.push(range.start);
            words.push(range.end - range.start);
            words.push(0b111);  // TODO
            words.push(0);
        }
    }

    // Peripheral regions
    for p in peripherals.values() {
        words.push(p.address);
        words.push(p.size);
        words.push(0b1011); // TODO
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
                let range = data.address.0..data.address.0 + data.data.len() as u32;
                if let Some(overlap) = output.range(range.clone()).next() {
                    return Err(format!("{}: record address range {:x?} overlaps {}",
                            input.display(), range, overlap.0).into());
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
    use goblin::elf::program_header::{PT_LOAD};

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
            return Err(format!("{}: record address range {:x?} overlaps {}",
                    input.display(), range, overlap.0).into());
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

