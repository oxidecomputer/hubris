// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::convert::TryInto;
use std::path::{Path, PathBuf};

use anyhow::bail;
use goblin::Object;
use indexmap::IndexMap;

use crate::Config;

fn armv7m_suggest(size: u64) -> u64 {
    size.next_power_of_two()
}
fn armv8m_suggest(size: u64) -> u64 {
    // Nearest chunk of 32
    ((size + 31) / 32) * 32
}

pub fn run(cfg: &Path) -> anyhow::Result<()> {
    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

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

    let suggest = if toml.target.starts_with("thumbv7") {
        armv7m_suggest
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
                // The PhysAddr is always a flash address, meaning we use the
                // "on-disk" size of this section.
                if let Some(region) = output_region(phdr.p_paddr) {
                    *memory_sizes.entry(region).or_default() += phdr.p_filesz;
                }
                // If the VirtAddr disagrees with the PhysAddr, then this is a
                // section which is relocated into RAM, so we also accumulate
                // its MemSiz.
                if phdr.p_paddr != phdr.p_vaddr {
                    let region = output_region(phdr.p_vaddr).unwrap();
                    *memory_sizes.entry(region).or_default() += phdr.p_memsz;
                }
            }
            *memory_sizes.entry("ram").or_default() += stacksize as u64;

            println!("{}", name);
            let mut my_suggestions = Vec::new();
            let mut has_asterisk = false;
            for (mem_name, used) in memory_sizes {
                let asterisk = name == "kernel" && mem_name == "ram";
                has_asterisk |= asterisk;
                if let Some(&size) = requires.get(mem_name) {
                    let percent = used * 100 / size as u64;
                    println!(
                        "  {:<6} {: >5} bytes ({}%){}",
                        format!("{}:", mem_name),
                        used,
                        percent,
                        if asterisk { "*" } else { "" },
                    );
                    let suggestion = suggest(used);
                    if suggestion != size as u64 && !asterisk {
                        my_suggestions.push((mem_name, size, suggestion));
                    }
                } else {
                    assert!(used == 0);
                }
            }
            // TODO: remove this once PR #393 is merged
            if has_asterisk {
                println!("  * kernel uses spare RAM for dynamic allocation");
            }
            if !my_suggestions.is_empty() {
                suggestions.push((name.to_owned(), my_suggestions));
            }
            Ok(())
        };

    check_task("kernel", toml.stacksize.unwrap(), &toml.kernel.requires)?;
    for (name, task) in &toml.tasks {
        println!();
        check_task(
            &name,
            task.stacksize.unwrap_or_else(|| toml.stacksize.unwrap()),
            &task.requires,
        )?;
    }

    if !suggestions.is_empty() {
        let mut savings: IndexMap<_, u64> = IndexMap::new();
        println!("\n========== Suggested changes ==========");
        for (name, list) in suggestions {
            println!("{}:", name);
            for (mem, prev, value) in list {
                println!(
                    "  {:<6} {} (from {})",
                    format!("{}:", mem),
                    value,
                    prev
                );
                *savings.entry(mem).or_default() += prev as u64 - value;
            }
        }
        println!("\n------------ Total savings ------------");
        for (mem, savings) in &savings {
            println!("  {:<6} {}", format!("{}:", mem), savings);
        }
    }

    Ok(())
}
