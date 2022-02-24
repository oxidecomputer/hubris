// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::convert::TryInto;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::bail;
use goblin::Object;
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
pub fn run(cfg: &Path, only_suggest: bool) -> anyhow::Result<()> {
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

    let toml = Config::from_file(&cfg)?;

    let mut dist_dir = PathBuf::from("target");
    dist_dir.push(&toml.name);
    dist_dir.push("dist");

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

    let mut suggestions = Vec::new();
    let mut check_task =
        |name: &str, stacksize: u32, requires: &IndexMap<String, u32>| {
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

            if !only_suggest {
                writeln!(out, "{}", name)?;
            }
            let mut my_suggestions = Vec::new();
            for (mem_name, used) in memory_sizes {
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
                    let suggestion = suggest(used);
                    if suggestion < size as u64 {
                        my_suggestions.push((mem_name, size, suggestion));
                    }
                } else {
                    assert!(used == 0);
                }
            }
            if !my_suggestions.is_empty() {
                suggestions.push((name.to_owned(), my_suggestions));
            }
            Ok(())
        };

    check_task(
        "kernel",
        toml.kernel.stacksize.unwrap_or(DEFAULT_KERNEL_STACK),
        &toml.kernel.requires,
    )?;
    for (name, task) in &toml.tasks {
        if !only_suggest {
            log::info!("");
        }
        check_task(
            &name,
            task.stacksize.or(toml.stacksize).unwrap(),
            &task.requires,
        )?;
    }

    if !suggestions.is_empty() {
        let mut savings: IndexMap<_, u64> = IndexMap::new();

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
            writeln!(out, "{}:", name)?;
            for (mem, prev, value) in list {
                write!(out, "  {:<6} {: >5}", format!("{}:", mem), value)?;
                out.set_color(ColorSpec::new().set_dimmed(true))?;
                writeln!(out, " (currently {})", prev)?;
                out.reset()?;
                *savings.entry(mem).or_default() += prev as u64 - value;
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
