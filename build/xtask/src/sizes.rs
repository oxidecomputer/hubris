// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashSet;
use std::convert::TryInto;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;

use anyhow::bail;
use goblin::Object;
use indexmap::map::Entry;
use indexmap::IndexMap;
use termcolor::{Color, ColorSpec, WriteColor};

use crate::{dist::DEFAULT_KERNEL_STACK, Config};

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
) -> anyhow::Result<()> {
    let toml = Config::from_file(&cfg)?;

    let sizes = create_sizes(cfg)?;

    let filename = format!("{}.json", toml.name);

    if save {
        println!("Writing json to {}", filename);
        fs::write(filename, serde_json::ser::to_string(&sizes)?)?;
        process::exit(0);
    } else if compare {
        let compare = fs::read(filename)?;
        let compare: (
            IndexMap<String, IndexMap<String, u64>>,
            IndexMap<String, Vec<Vec<(String, u32, u64)>>>,
        ) = serde_json::from_slice(&compare)?;

        compare_sizes(sizes, compare)?;
        process::exit(0);
    }

    let (sizes, suggestions) = sizes;

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
        |name: &str, requires: &IndexMap<String, u32>| -> anyhow::Result<()> {
            let sizes = &sizes[name];

            if !only_suggest {
                writeln!(out, "{}", name)?;
            }

            for (mem_name, &used) in sizes {
                if let Some(&size) = requires.get(mem_name) {
                    let percent = used * 100 / size as u64;
                    if !only_suggest {
                        write!(
                            out,
                            "  {:<6} {: >5} bytes",
                            format!("{}:", mem_name),
                            used,
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
                        write!(out, " ({}%)", percent,)?;
                        out.reset()?;
                        writeln!(out)?;
                    }
                } else {
                    assert!(used == 0);
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

    Ok(())
}

fn create_sizes(
    cfg: &Path,
) -> anyhow::Result<(
    IndexMap<String, IndexMap<String, u64>>,
    IndexMap<String, Vec<Vec<(String, u32, u64)>>>,
)> {
    let toml = Config::from_file(&cfg)?;

    let dist_dir = Path::new("target").join(&toml.name).join("dist");

    let mut memories = IndexMap::new();
    for (name, out) in &toml.outputs {
        // This is called after `dist`, which logs this overflow gracefully
        let end = out.address.checked_add(out.size).unwrap();
        memories.insert(name.clone(), out.address..end);
    }

    let output_region = |vaddr: u64| {
        memories
            .iter()
            .find(|(_, region)| region.contains(&vaddr.try_into().unwrap()))
            .map(|(name, _)| name.as_str())
    };

    let suggest = if toml.target.starts_with("thumbv7")
        || toml.target.starts_with("thumbv6m")
    {
        pow2_suggest
    } else if toml.target.starts_with("thumbv8m") {
        armv8m_suggest
    } else {
        panic!("Unknown target: {}", toml.target);
    };

    let check_task = move |name: &str,
                           stacksize: u32,
                           requires: &IndexMap<String, u32>|
          -> anyhow::Result<(
        IndexMap<String, u64>,
        Vec<Vec<(String, u32, u64)>>,
    )> {
        let mut suggestions = Vec::new();
        let mut sizes = IndexMap::new();

        let mut elf_name = dist_dir.clone();
        elf_name.push(name);
        let buffer = std::fs::read(elf_name)?;
        let elf = match Object::parse(&buffer)? {
            Object::Elf(elf) => elf,
            o => bail!("Invalid Object {:?}", o),
        };

        let mut memory_sizes = IndexMap::new();
        for phdr in &elf.program_headers {
            if let Some(vregion) = output_region(phdr.p_vaddr) {
                *memory_sizes.entry(vregion).or_default() += phdr.p_memsz;
            }
            // If the VirtAddr disagrees with the PhysAddr, then this is a
            // section which is relocated into RAM, so we also accumulate
            // its FileSiz in the physical address (which is presumably
            // flash).
            if phdr.p_vaddr != phdr.p_paddr {
                let region = output_region(phdr.p_paddr).unwrap();
                *memory_sizes.entry(region).or_default() += phdr.p_filesz;
            }
        }
        *memory_sizes.entry("ram").or_default() += stacksize as u64;

        let mut my_suggestions = Vec::new();

        for (mem_name, used) in memory_sizes {
            if let Some(&size) = requires.get(mem_name) {
                sizes.insert(mem_name.to_string(), used);
                let suggestion = suggest(used);
                if suggestion < size as u64 {
                    my_suggestions.push((
                        mem_name.to_string(),
                        size,
                        suggestion,
                    ));
                }
            } else {
                assert!(used == 0);
            }
        }
        if !my_suggestions.is_empty() {
            suggestions.push(my_suggestions);
        }
        Ok((sizes, suggestions))
    };

    let mut sizes: IndexMap<String, IndexMap<String, u64>> = IndexMap::new();
    let mut suggestions: IndexMap<String, Vec<Vec<(String, u32, u64)>>> =
        IndexMap::new();

    let kernel_sizes = check_task(
        "kernel",
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &toml.kernel.requires,
    )?;
    sizes.insert(String::from("kernel"), kernel_sizes.0);
    suggestions.insert(String::from("kernel"), kernel_sizes.1);

    for (name, task) in &toml.tasks {
        let task_sizes = check_task(
            &name,
            task.stacksize.or(toml.stacksize).unwrap(),
            &task.requires,
        )?;

        sizes.insert(name.to_string(), task_sizes.0);
        suggestions.insert(name.to_string(), task_sizes.1);
    }

    Ok((sizes, suggestions))
}

fn compare_sizes(
    current_sizes: (
        IndexMap<String, IndexMap<String, u64>>,
        IndexMap<String, Vec<Vec<(String, u32, u64)>>>,
    ),
    saved_sizes: (
        IndexMap<String, IndexMap<String, u64>>,
        IndexMap<String, Vec<Vec<(String, u32, u64)>>>,
    ),
) -> anyhow::Result<()> {
    println!("Comparing against the previously saved sizes");

    // 0 is sizes, 1 is suggestions
    let mut current_sizes = current_sizes.0;
    let mut saved_sizes = saved_sizes.0;

    let mut names: HashSet<String> = current_sizes.keys().cloned().collect();
    names.extend(saved_sizes.keys().cloned());

    for name in names {
        println!("Checking for differences in {}", name);

        let current_size = current_sizes.entry(name.clone());
        let saved_size = saved_sizes.entry(name.clone());

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
