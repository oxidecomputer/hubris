// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;

use anyhow::{bail, Context, Result};
use colored::*;
use goblin::Object;
use indexmap::map::Entry;
use indexmap::IndexMap;

use crate::{
    dist::{
        get_max_stack, Allocations, ContiguousRanges, DEFAULT_KERNEL_STACK,
    },
    Config,
};

#[derive(Debug)]
struct TaskSizes<'a> {
    /// Represents a map of task name -> memory region -> bytes used
    sizes: IndexMap<&'a str, IndexMap<&'a str, u64>>,
}

/// When `only_suggest` is true, prints only the suggested improvements to
/// stderr, rather than printing all sizes.  Suggestions are formatted to
/// match compiler warnings.
pub fn run(
    cfg: &Path,
    allocs: &Allocations,
    only_suggest: bool,
    compare: bool,
    save: bool,
    verbose: bool,
) -> Result<()> {
    let toml = Config::from_file(cfg)?;
    let sizes = create_sizes(&toml)?;

    let filename = format!("{}.json", toml.name);

    if save {
        println!("Writing json to {filename}");
        fs::write(filename, serde_json::ser::to_string(&sizes.sizes)?)?;
        process::exit(0);
    } else if compare {
        let compare = fs::read(filename)?;
        let compare: IndexMap<&str, IndexMap<&str, u64>> =
            serde_json::from_slice(&compare)?;
        let compare = TaskSizes { sizes: compare };

        compare_sizes(sizes, compare)?;
        process::exit(0);
    }

    let mut out: Box<dyn Write> = if only_suggest {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    };

    // Print detailed sizes relative to usage
    if !only_suggest {
        let map = build_memory_map(&toml, &sizes, allocs)?;
        print_memory_map(&toml, &map, verbose)?;
        print!("\n\n");
        print_task_table(&toml, &map)?;
        print!("\n\n");
    }
    // Always do stack size estimation, but only print the stacks if we are in
    // verbose mode.
    let stacks = estimate_task_stacks(&toml, !only_suggest)?;

    // Because tasks are autosized, the only place where we can improve
    // memory allocation is in the kernel...
    let mut printed_header = false;
    let mut printed_name = false;
    let mut maybe_print_header = |out: &mut dyn Write| -> std::io::Result<()> {
        if !printed_header {
            printed_header = true;
            if only_suggest {
                write!(out, "{}", "warning".bold().yellow())?;
                writeln!(out, ": memory allocation is sub-optimal")?;
                writeln!(out, "{}", "Suggested improvements:".bold())?;
            } else {
                writeln!(
                    out,
                    "{}",
                    "\n========== Suggested changes ==========".bold()
                )?;
            }
        }
        Ok(())
    };

    for (mem, &used) in sizes.sizes["kernel"].iter() {
        if used == 0 {
            continue;
        }
        let size = toml.kernel.requires[&mem.to_string()];

        let suggestion = toml.suggest_memory_region_size("kernel", used, 1);
        assert_eq!(suggestion.len(), 1, "kernel should not be > 1 region");
        let suggestion = suggestion[0];

        if suggestion >= size as u64 {
            continue;
        }
        maybe_print_header(&mut out)?;
        if !printed_name {
            printed_name = true;
            writeln!(out, "kernel:")?;
        }
        writeln!(
            out,
            "  {:<6} {: >5} {}",
            format!("{mem}:"),
            suggestion,
            format!(" (currently {size})").dimmed()
        )?;
    }

    // ... and also stack sizes that are over margin.
    let mut total_free_real_estate = 0;
    for (task_name, stack) in stacks {
        let total_ram = sizes.sizes[task_name]["ram"];
        let oldlim = stack.limit;
        let nonstack_ram = total_ram.saturating_sub(oldlim);
        let newlim = stack.max_estimate + 8;
        let newram = nonstack_ram + newlim;

        // Only claim that there's free real estate if we could shrink the power
        // of two memory region by resizing the task's RAM request.
        let old_pow2 = total_ram.next_power_of_two();
        let new_pow2 = newram.next_power_of_two();
        if new_pow2 < old_pow2 {
            let free_real_estate = old_pow2 - new_pow2;
            maybe_print_header(&mut out)?;
            writeln!(out, "{task_name}:")?;
            writeln!(
                out,
                "  {:<6} {newlim: >5} {}",
                "stack",
                format!(" (currently {oldlim})").dimmed()
            )?;
            writeln!(
                out,
                "  {:<6} {newram: >5} {}{}",
                "ram",
                format!(" (currently {total_ram})").dimmed(),
                format!(" !!! {free_real_estate}B of free real estate !!!")
                    .dimmed()
            )?;
            total_free_real_estate += free_real_estate;
        }
    }

    if total_free_real_estate > 0 {
        writeln!(
            out,
            "\nthere may be up to {total_free_real_estate} bytes of free \
             real estate in this image,\nif you're brave enough to mess with \
             stack sizes!\n"
        )?;
    }

    Ok(())
}

#[derive(Copy, Clone, Debug)]
enum Recommended {
    FixedSize,
    MaxSize(u32),
}
#[derive(Clone, Debug)]
struct MemoryChunk<'a> {
    used_size: u64,
    total_size: Vec<u32>,
    owner: &'a str,
    recommended: Option<Recommended>,
}

fn build_memory_map<'a>(
    toml: &'a Config,
    sizes: &'a TaskSizes,
    allocs: &'a Allocations,
) -> Result<BTreeMap<&'a str, BTreeMap<u32, MemoryChunk<'a>>>> {
    let mut map: BTreeMap<&str, BTreeMap<u32, MemoryChunk>> = BTreeMap::new();

    for (name, requires, alloc) in toml
        .tasks
        .iter()
        .map(|(name, task)| {
            (
                name.as_str(),
                task.max_sizes.clone(),
                allocs.tasks[name].clone(),
            )
        })
        .chain(std::iter::once((
            "kernel",
            toml.kernel.requires.clone(),
            allocs
                .kernel
                .iter()
                .map(|(name, v)| {
                    (name.to_owned(), ContiguousRanges::new(v.clone()))
                })
                .collect(),
        )))
        .chain(allocs.caboose.iter().map(|(region, size)| {
            let mut alloc = BTreeMap::new();
            alloc.insert(region.clone(), ContiguousRanges::new(size.clone()));
            let mut requires = IndexMap::new();
            requires
                .insert(region.clone(), toml.caboose.as_ref().unwrap().size);
            ("-caboose-", requires, alloc)
        }))
    {
        // Here's the minimal size, based on the temporarily linked file
        let sizes = &sizes.sizes[name];
        for (mem_name, &used) in sizes {
            if used == 0 {
                continue;
            }
            let alloc = &alloc[&mem_name.to_string()];
            map.entry(mem_name).or_default().insert(
                alloc.start(),
                MemoryChunk {
                    used_size: used,
                    total_size: alloc.iter().map(|v| v.end - v.start).collect(),
                    owner: name,
                    recommended: requires
                        .get(mem_name.to_owned())
                        .cloned()
                        .map(match name {
                            "kernel" => |_| Recommended::FixedSize,
                            _ => Recommended::MaxSize,
                        }),
                },
            );
        }
    }
    Ok(map)
}

fn print_task_table(
    toml: &Config,
    map: &BTreeMap<&str, BTreeMap<u32, MemoryChunk>>,
) -> Result<()> {
    let task_pad = toml
        .tasks
        .keys()
        .map(|s| s.as_str())
        .chain(std::iter::once("PROGRAM"))
        .map(|k| k.len())
        .max()
        .unwrap_or(0);
    let mem_pad = map
        .values()
        .flat_map(|m| m.values())
        .map(|c| format!("{}", c.total_size.iter().sum::<u32>()).len())
        .chain(std::iter::once(4))
        .max()
        .unwrap_or(0) as usize;
    let region_pad = map
        .keys()
        .chain(std::iter::once(&"REGION"))
        .map(|c| c.to_string().len())
        .max()
        .unwrap_or(0) as usize;

    // Turn the memory map around so we can index it by [region][task name]
    let map: BTreeMap<&str, BTreeMap<&str, MemoryChunk>> = map
        .iter()
        .map(|(region, map)| {
            (
                *region,
                map.values()
                    .map(|chunk| (chunk.owner, chunk.clone()))
                    .collect(),
            )
        })
        .collect();

    println!(
        "{:<task$}  {:<reg$}  {:<mem$}  {:<mem$}  LIMIT",
        "PROGRAM",
        "REGION",
        "USED",
        "SIZE",
        task = task_pad,
        reg = region_pad,
        mem = mem_pad,
    );

    for name in
        std::iter::once("kernel").chain(toml.tasks.keys().map(|k| k.as_str()))
    {
        let mut printed_name = false;
        for (region, map) in &map {
            if let Some(chunk) = map.get(name) {
                print!(
                    "{:<task$}  ",
                    if !printed_name { name } else { "" },
                    task = task_pad
                );
                printed_name = true;
                print!("{region:<region_pad$}  ");
                print!(
                    "{:<mem$}  {:<mem$}  ",
                    chunk.used_size,
                    chunk.total_size.iter().sum::<u32>(),
                    mem = mem_pad,
                );
                match chunk.recommended {
                    None => print!("(auto)"),
                    Some(Recommended::MaxSize(m)) => print!("{m}"),
                    Some(Recommended::FixedSize) => print!("(fixed)"),
                }
                println!();
            }
        }
    }
    Ok(())
}

fn print_memory_map(
    toml: &Config,
    map: &BTreeMap<&str, BTreeMap<u32, MemoryChunk>>,
    verbose: bool,
) -> Result<()> {
    let task_pad = toml
        .tasks
        .keys()
        .map(|s| s.as_str())
        .chain(std::iter::once("-padding-"))
        .map(|k| k.len())
        .max()
        .unwrap_or(0);
    let mem_pad = map
        .values()
        .flat_map(|m| m.values())
        .map(|c| format!("{}", c.total_size.iter().sum::<u32>()).len())
        .max()
        .unwrap_or(0);
    for (mem_name, map) in map {
        println!("\n{mem_name}:");
        if verbose {
            println!(
            "      ADDRESS  | {:^task$} | {:>mem$} | {:>mem$} | {:>mem$} | LIMIT",
            "PROGRAM",
            "USED",
            "SIZE",
            "CHUNKS",
            task = task_pad,
            mem = mem_pad,
        );
        } else {
            println!(
                "      ADDRESS  | {:^task$} | {:>mem$} | {:>mem$} | LIMIT",
                "PROGRAM",
                "USED",
                "SIZE",
                task = task_pad,
                mem = mem_pad,
            );
        }

        let next = map.keys().skip(1).map(Some).chain(std::iter::once(None));
        for ((start, chunk), next) in map.iter().zip(next) {
            for (i, mem) in chunk.total_size.iter().enumerate() {
                print!(
                    "    {:#010x} | ",
                    start + chunk.total_size[0..i].iter().sum::<u32>()
                );
                if verbose {
                    if i == 0 {
                        print!(
                            "{:<size$} | {:>mem$} | {:>mem$} | {:>mem$} | ",
                            chunk.owner,
                            chunk.used_size,
                            chunk.total_size.iter().sum::<u32>(),
                            mem,
                            size = task_pad,
                            mem = mem_pad,
                        );
                    } else {
                        print!(
                            "{:<size$} | {:>mem$} | {:>mem$} | {:>mem$} | ",
                            "",
                            "",
                            "",
                            mem,
                            size = task_pad,
                            mem = mem_pad,
                        );
                    }
                } else {
                    print!(
                        "{:<size$} | {:>mem$} | {:>mem$} | ",
                        chunk.owner,
                        chunk.used_size,
                        chunk.total_size.iter().sum::<u32>(),
                        size = task_pad,
                        mem = mem_pad,
                    );
                }
                if i == 0 {
                    match chunk.recommended {
                        None => print!("(auto)"),
                        Some(Recommended::MaxSize(m)) => print!("{m}"),
                        Some(Recommended::FixedSize) => print!("(fixed)"),
                    }
                }
                println!();
                // Only print the header if we're not being verbose
                if !verbose {
                    break;
                }
            }

            // Print padding, if relevant
            let chunk_size = chunk.total_size.iter().sum::<u32>();
            if let Some(&next) = next {
                if next != start + chunk_size {
                    print!("    {:#010x} | ", start + chunk_size);
                    if verbose {
                        println!(
                            "{:<size$} | {:>mem$} | {:>mem$} | {:>mem$} |",
                            "-padding-",
                            "--",
                            next - (start + chunk_size),
                            "--",
                            size = task_pad,
                            mem = mem_pad,
                        );
                    } else {
                        println!(
                            "{:<size$} | {:>mem$} | {:>mem$} | ",
                            "-padding-",
                            "--",
                            next - (start + chunk_size),
                            size = task_pad,
                            mem = mem_pad,
                        );
                    }
                }
            } else {
                print!("    {:#010x} | ", start + chunk_size);
                if verbose {
                    println!(
                        "{:<size$} | {:>mem$} | {:>mem$} | {:>mem$} | ",
                        "--end--",
                        "",
                        "",
                        "",
                        size = task_pad,
                        mem = mem_pad,
                    );
                } else {
                    println!(
                        "{:<size$} | {:>mem$} | {:>mem$} | ",
                        "--end--",
                        "",
                        "",
                        size = task_pad,
                        mem = mem_pad,
                    );
                }
            }
        }
    }
    Ok(())
}

struct Stacks {
    max_estimate: u64,
    limit: u64,
}

fn estimate_task_stacks<'a>(
    toml: &'a Config,
    print: bool,
) -> Result<IndexMap<&'a str, Stacks>> {
    let mut tasks = IndexMap::with_capacity(toml.tasks.len());
    for (i, (task_name, task)) in toml.tasks.iter().enumerate() {
        let task_stack_size =
            task.stacksize.unwrap_or_else(|| toml.stacksize.unwrap());

        let max_stack = get_max_stack(toml, task_name, false)?;
        let total: u64 = max_stack.iter().map(|(n, _)| *n).sum();
        if print {
            println!("{task_name}: {total} bytes (limit is {task_stack_size})");
            for (frame_size, name) in max_stack {
                let s = format!("[+{frame_size}]");
                println!("  {s:>7} {name}");
            }
        }
        if i + 1 < toml.tasks.len() {
            println!();
        }
        tasks.insert(
            task_name.as_ref(),
            Stacks {
                max_estimate: total,
                limit: task_stack_size as u64,
            },
        );
    }
    Ok(tasks)
}

/// Loads the size of the given task (or kernel)
pub fn load_task_size<'a>(
    toml: &'a Config,
    name: &str,
    stacksize: u32,
) -> Result<IndexMap<&'a str, u64>> {
    // Load the .tmp file (which does not have flash fill) for everything
    // except the kernel
    let elf_name =
        Path::new("target")
            .join(&toml.name)
            .join("dist")
            .join(match name {
                "kernel" => name.to_owned(),
                _ => format!("{name}.tmp"),
            });
    let buffer = std::fs::read(elf_name)?;
    let elf = match Object::parse(&buffer)? {
        Object::Elf(elf) => elf,
        o => bail!("Invalid Object {:?}", o),
    };

    // We can't naively add up section sizes, since there may be gaps left
    // by alignment requirements.  Instead, we track the min and max bounds
    // within each memory region (flash, RAM, etc), then extract the sizes
    // afterwards.
    let mut memory_sizes = IndexMap::new();
    let mut record_size = |start, size| {
        if let Some(region) = toml.output_region(start) {
            let end = start + size;
            let r = memory_sizes.entry(region).or_insert_with(|| start..end);
            r.start = r.start.min(start);
            r.end = r.end.max(end);
            Ok(())
        } else {
            bail!("could not find region at {start}");
        }
    };
    for phdr in &elf.program_headers {
        if phdr.p_type != goblin::elf::program_header::PT_LOAD {
            continue;
        }
        record_size(phdr.p_vaddr, phdr.p_memsz)?;

        // If the VirtAddr disagrees with the PhysAddr, then this is a
        // section which is relocated into RAM, so we also accumulate
        // its FileSiz in the physical address (which is presumably
        // flash).
        if phdr.p_vaddr != phdr.p_paddr {
            record_size(phdr.p_paddr, phdr.p_filesz).with_context(|| {
                format!("Failed to remap relocated section at {}", phdr.p_paddr)
            })?;
        }
    }

    let mut memory_sizes: IndexMap<&str, u64> = memory_sizes
        .into_iter()
        .map(|(name, range)| (name, range.end - range.start))
        .collect();

    // The stack is 8-byte aligned (checked elsewhere in the build and
    // rechecked here) Everything else in RAM is ALIGN(4), so we don't need to
    // worry about padding here.
    assert!(stacksize.trailing_zeros() >= 3);
    *memory_sizes.entry("ram").or_default() += stacksize as u64;

    Ok(memory_sizes)
}

fn create_sizes(toml: &Config) -> Result<TaskSizes<'_>> {
    let mut sizes = IndexMap::new();

    let kernel_sizes = load_task_size(
        toml,
        "kernel",
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
    )?;
    sizes.insert("kernel", kernel_sizes);

    for (name, task) in &toml.tasks {
        let stacksize = task.stacksize.or(toml.stacksize).unwrap();
        let task_sizes = load_task_size(toml, name, stacksize)?;

        sizes.insert(name, task_sizes);
    }

    if let Some(caboose) = &toml.caboose {
        let mut map = IndexMap::new();
        map.insert(caboose.region.as_str(), caboose.size as u64);
        sizes.insert("-caboose-", map);
    }

    Ok(TaskSizes { sizes })
}

fn compare_sizes(
    current_sizes: TaskSizes,
    saved_sizes: TaskSizes,
) -> Result<()> {
    println!("Comparing against the previously saved sizes");

    let mut current_sizes = current_sizes.sizes;
    let mut saved_sizes = saved_sizes.sizes;

    let names: BTreeSet<&str> = current_sizes
        .keys()
        .chain(saved_sizes.keys())
        .cloned()
        .collect();

    for name in names {
        println!("Checking for differences in {name}");

        let current_size = current_sizes.entry(name);
        let saved_size = saved_sizes.entry(name);

        match (current_size, saved_size) {
            // the common case; both are in both
            (Entry::Occupied(current_entry), Entry::Occupied(saved_entry)) => {
                let current = current_entry.get();
                let saved = saved_entry.get();
                for (key, &value) in current {
                    let saved = saved.get(key).cloned().unwrap_or_default();
                    let diff = value as i64 - saved as i64;

                    if diff != 0 {
                        println!("\t{key}: {diff}");
                    }
                }
            }
            // we have added something new
            (Entry::Vacant(_), Entry::Occupied(saved_entry)) => {
                println!(
                    "This task was added since we last saved size information."
                );
                let saved = saved_entry.get();
                for (key, value) in saved {
                    println!("\t{key}: {value}");
                }
            }
            // we have removed this entirely
            (Entry::Occupied(current_entry), Entry::Vacant(_)) => {
                println!("This task was removed since we last saved size information.");
                let current = current_entry.get();
                for (key, value) in current {
                    println!("\t{key}: {value}");
                }
            }
            // this should never happen
            (Entry::Vacant(_), Entry::Vacant(_)) => {
                bail!("{} doesn't exist, and this should never happen.", name)
            }
        }
    }

    Ok(())
}
