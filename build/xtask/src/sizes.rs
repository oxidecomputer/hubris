// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeSet;
use std::convert::TryInto;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;

use anyhow::{bail, Result};
use goblin::Object;
use indexmap::map::Entry;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use termcolor::{Color, ColorSpec, WriteColor};

use crate::{dist::DEFAULT_KERNEL_STACK, Config};

#[derive(Copy, Clone, Debug, Serialize, Deserialize, Default)]
struct Usage {
    /// Actual memory usage
    bytes: u64,
    /// Amount of memory requested in the TOML file, or `None`
    required: Option<u64>,
}

#[derive(Debug, Serialize)]
struct TaskSizes<'a> {
    /// Represents a map of task name -> memory region -> bytes used
    sizes: IndexMap<&'a str, IndexMap<&'a str, Usage>>,
}

fn pow2_suggest(size: u64) -> u64 {
    size.next_power_of_two()
}

fn armv8m_suggest(size: u64) -> u64 {
    // Nearest chunk of 32
    ((size + 31) / 32) * 32
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
        let compare: IndexMap<&str, IndexMap<&str, Usage>> =
            serde_json::from_slice(&compare)?;
        let compare = TaskSizes { sizes: compare };

        compare_sizes(sizes, compare)?;
        process::exit(0);
    }

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

    let toml = Config::from_file(cfg)?;
    let mut print_task_sizes =
        |name: &str, requires: &IndexMap<String, u32>| -> Result<()> {
            let sizes = &sizes.sizes[name];

            if !only_suggest {
                writeln!(out, "{}", name)?;
            }

            for (mem_name, &used) in sizes {
                if let Some(size) = used.required {
                    let percent = used.bytes * 100 / size as u64;
                    if !only_suggest {
                        write!(
                            out,
                            "  {:<6} {: >5} bytes",
                            format!("{}:", mem_name),
                            used.bytes,
                        )?;
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
                        out.reset()?;
                        writeln!(out)?;
                    }
                } else {
                    assert!(used.bytes == 0);
                }
            }

            Ok(())
        };

    print_task_sizes("kernel", &toml.kernel.requires)?;

    for (name, task) in &toml.tasks {
        if !only_suggest {
            println!();
        }
        print_task_sizes(&name, &task.requires)?;
    }
    /*
    if !suggestions.is_empty() {
        let mut savings: IndexMap<String, u64> = IndexMap::new();

        if only_suggest {
            out.set_color(
                ColorSpec::new().set_bold(true).set_fg(Some(Color::Yellow)),
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

        for (name, list) in suggestions {
            if list.is_empty() {
                continue;
            }
            writeln!(out, "{}:", name)?;
            for list in list {
                for (mem, prev, value) in list {
                    write!(out, "  {:<6} {: >5}", format!("{}:", mem), value)?;
                    out.set_color(ColorSpec::new().set_dimmed(true))?;
                    writeln!(out, " (currently {})", prev)?;
                    out.reset()?;
                    *savings.entry(mem).or_default() += prev as u64 - value;
                }
            }
        }

        if !only_suggest {
            out.set_color(ColorSpec::new().set_bold(true))?;
            writeln!(out, "\n------------ Total savings ------------")?;
            out.reset()?;
            for (mem, savings) in &savings {
                writeln!(out, "  {:<6} {}", format!("{}:", mem), savings)?;
            }
        }
    }
    */

    Ok(())
}

/// Loads the size of the given task (or kernel)
fn load_task_size<'a>(
    toml: &'a Config,
    name: &str,
    stacksize: u32,
    requires: &IndexMap<String, u32>,
) -> Result<IndexMap<&'a str, Usage>> {
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

    let mut memory_sizes: IndexMap<&str, Usage> = IndexMap::new();
    for phdr in &elf.program_headers {
        if let Some(vregion) = toml.output_region(phdr.p_vaddr) {
            memory_sizes.entry(vregion).or_default().bytes += phdr.p_memsz;
        }
        // If the VirtAddr disagrees with the PhysAddr, then this is a
        // section which is relocated into RAM, so we also accumulate
        // its FileSiz in the physical address (which is presumably
        // flash).
        if phdr.p_vaddr != phdr.p_paddr {
            let region = toml.output_region(phdr.p_paddr).unwrap();
            memory_sizes.entry(region).or_default().bytes += phdr.p_filesz;
        }
    }
    memory_sizes.entry("ram").or_default().bytes += stacksize as u64;

    for (mem_name, used) in memory_sizes.iter_mut() {
        used.required = requires.get(*mem_name).map(|i| *i as u64)
    }

    Ok(memory_sizes)
}

fn create_sizes(toml: &Config) -> Result<TaskSizes> {
    let mut sizes = IndexMap::new();

    let kernel_sizes = load_task_size(
        &toml,
        "kernel",
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &toml.kernel.requires,
    )?;
    sizes.insert("kernel", kernel_sizes);

    for (name, task) in &toml.tasks {
        let stacksize = task.stacksize.or(toml.stacksize).unwrap();
        let task_sizes =
            load_task_size(&toml, &name, stacksize, &task.requires)?;

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
                    let diff = value.bytes as i64 - saved.bytes as i64;

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
                    println!("\t{}: {}", key, value.bytes);
                }
            }
            // we have removed this entirely
            (Entry::Occupied(current_entry), Entry::Vacant(_)) => {
                println!("This task was removed since we last saved size information.");
                let current = current_entry.get();
                for (key, value) in current {
                    println!("\t{}: {}", key, value.bytes);
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
