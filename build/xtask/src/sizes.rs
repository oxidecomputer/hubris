// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;

use anyhow::{bail, Result};
use goblin::Object;
use indexmap::map::Entry;
use indexmap::IndexMap;
use serde::Serialize;
use termcolor::{Color, ColorSpec, WriteColor};

use crate::{dist::DEFAULT_KERNEL_STACK, Config};

#[derive(Debug, Serialize)]
struct TaskSizes<'a> {
    /// Represents a map of task name -> memory region -> bytes used
    sizes: IndexMap<&'a str, IndexMap<&'a str, u64>>,
}

/// When `only_suggest` is true, prints only the suggested improvements to
/// stderr, rather than printing all sizes.  Suggestions are formatted to
/// match compiler warnings.
pub fn run(
    cfg: &Path,
    only_suggest: bool,
    compare: bool,
    save: bool,
) -> Result<()> {
    let toml = Config::from_file(&cfg)?;
    let sizes = create_sizes(&toml)?;

    let filename = format!("{}.json", toml.name);

    if save {
        println!("Writing json to {}", filename);
        fs::write(filename, serde_json::ser::to_string(&sizes)?)?;
        process::exit(0);
    } else if compare {
        let compare = fs::read(filename)?;
        let compare: IndexMap<&str, IndexMap<&str, u64>> =
            serde_json::from_slice(&compare)?;
        let compare = TaskSizes { sizes: compare };

        compare_sizes(sizes, compare)?;
        process::exit(0);
    }

    // Way too much setup to get output stream and colors set up
    let s = if only_suggest {
        atty::Stream::Stderr
    } else {
        atty::Stream::Stdout
    };
    let color_choice = if atty::is(s) {
        termcolor::ColorChoice::Auto
    } else {
        termcolor::ColorChoice::Never
    };
    let mut out_stream = match s {
        atty::Stream::Stderr => termcolor::StandardStream::stderr,
        atty::Stream::Stdout => termcolor::StandardStream::stdout,
        _ => panic!("Invalid stream"),
    }(color_choice);
    let out = &mut out_stream;

    // Helpful
    let names = std::iter::once("kernel")
        .chain(toml.tasks.keys().map(|name| name.as_str()));

    // Print detailed sizes relative to usage
    if !only_suggest {
        for name in names.clone() {
            writeln!(out, "\n{}", name)?;
            let requires = toml.requires(name);
            let sizes = &sizes.sizes[name];

            for (mem_name, &used) in sizes {
                if used == 0 && !requires.contains_key(&mem_name.to_string()) {
                    continue;
                }
                write!(
                    out,
                    "  {:<6} {: >5} bytes",
                    format!("{}:", mem_name),
                    used,
                )?;
                if let Some(&size) = requires.get(&mem_name.to_string()) {
                    let percent = used * 100 / size as u64;
                    let mut color = ColorSpec::new();
                    color.set_fg(Some(if percent >= 50 {
                        Color::Green
                    } else if percent > 25 {
                        Color::Yellow
                    } else {
                        Color::Red
                    }));
                    out.set_color(&color)?;
                    write!(out, " ({}%)", percent)?;

                    let autosize = toml.suggest_memory_region_size(name, used);
                    if size != autosize as u32 && name != "kernel" {
                        let mut color = ColorSpec::new();
                        color.set_fg(Some(Color::Blue));
                        out.set_color(&color)?;
                        write!(
                            out,
                            " [autosized to {}]",
                            toml.suggest_memory_region_size(name, used)
                        )?;
                    }
                } else if name != "kernel" {
                    let mut color = ColorSpec::new();
                    color.set_fg(Some(Color::Blue));
                    out.set_color(&color)?;
                    write!(
                        out,
                        " [autosized to {}]",
                        toml.suggest_memory_region_size(name, used)
                    )?;
                }
                out.reset()?;
                writeln!(out)?;
            }
        }
    }

    let mut printed_header = false;
    let mut savings: IndexMap<&str, u64> = IndexMap::new();
    for name in names.clone() {
        let requires = toml.requires(name);
        let sizes = &sizes.sizes[name];
        let mut printed_name = false;

        for (mem, &used) in sizes.iter() {
            let size = match requires.get(&mem.to_string()) {
                Some(s) => *s,
                _ => continue,
            };

            let suggestion = toml.suggest_memory_region_size(name, used);
            if suggestion >= size as u64 {
                continue;
            }
            if !printed_header {
                printed_header = true;
                if only_suggest {
                    out.set_color(
                        ColorSpec::new()
                            .set_bold(true)
                            .set_fg(Some(Color::Yellow)),
                    )?;
                    write!(out, "warning")?;
                    out.reset()?;
                    writeln!(out, ": memory allocation is sub-optimal")?;
                    out.set_color(ColorSpec::new().set_bold(true))?;
                    writeln!(out, "Suggested improvements:")?;
                    out.reset()?;
                } else {
                    out.set_color(ColorSpec::new().set_bold(true))?;
                    writeln!(out, "\n========== Suggested changes ==========")?;
                    out.reset()?;
                }
            }
            if !printed_name {
                printed_name = true;
                writeln!(out, "{}:", name)?;
            }
            write!(out, "  {:<6} {: >5}", format!("{}:", mem), suggestion)?;
            out.set_color(ColorSpec::new().set_dimmed(true))?;
            if name == "kernel" {
                writeln!(out, " (currently {})", size)?;
                *savings.entry(mem).or_default() += size as u64 - suggestion;
            } else {
                writeln!(out, " (autosized with max {})", size)?;
            }
            out.reset()?;
        }
    }

    if !only_suggest && !savings.is_empty() {
        out.set_color(ColorSpec::new().set_bold(true))?;
        writeln!(out, "\n------------ Total savings ------------")?;
        out.reset()?;
        for (mem, savings) in &savings {
            writeln!(out, "  {:<6} {}", format!("{}:", mem), savings)?;
        }
    }

    Ok(())
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
                _ => format!("{}.tmp", name),
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
            true
        } else {
            false
        }
    };
    for phdr in &elf.program_headers {
        record_size(phdr.p_vaddr, phdr.p_memsz);

        // If the VirtAddr disagrees with the PhysAddr, then this is a
        // section which is relocated into RAM, so we also accumulate
        // its FileSiz in the physical address (which is presumably
        // flash).
        if phdr.p_vaddr != phdr.p_paddr {
            if !record_size(phdr.p_paddr, phdr.p_filesz) {
                bail!("Failed to remap relocated section at {}", phdr.p_paddr);
            }
        }
    }
    let mut memory_sizes: IndexMap<&str, u64> = memory_sizes
        .into_iter()
        .map(|(name, range)| (name, range.end - range.start))
        .collect();

    // XXX: are there alignment issues with the stack here?
    *memory_sizes.entry("ram").or_default() += stacksize as u64;

    Ok(memory_sizes)
}

fn create_sizes(toml: &Config) -> Result<TaskSizes> {
    let mut sizes = IndexMap::new();

    let kernel_sizes = load_task_size(
        &toml,
        "kernel",
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
    )?;
    sizes.insert("kernel", kernel_sizes);

    for (name, task) in &toml.tasks {
        let stacksize = task.stacksize.or(toml.stacksize).unwrap();
        let task_sizes = load_task_size(&toml, &name, stacksize)?;

        sizes.insert(name, task_sizes);
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
        println!("Checking for differences in {}", name);

        let current_size = current_sizes.entry(&name);
        let saved_size = saved_sizes.entry(&name);

        match (current_size, saved_size) {
            // the common case; both are in both
            (Entry::Occupied(current_entry), Entry::Occupied(saved_entry)) => {
                let current = current_entry.get();
                let saved = saved_entry.get();
                for (key, &value) in current {
                    let saved = saved.get(key).cloned().unwrap_or_default();
                    let diff = value as i64 - saved as i64;

                    if diff != 0 {
                        println!("\t{}: {}", key, diff);
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
                    println!("\t{}: {}", key, value);
                }
            }
            // we have removed this entirely
            (Entry::Occupied(current_entry), Entry::Vacant(_)) => {
                println!("This task was removed since we last saved size information.");
                let current = current_entry.get();
                for (key, value) in current {
                    println!("\t{}: {}", key, value);
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
