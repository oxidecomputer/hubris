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
    // Nearest power of two
    let mut i = 1;
    while i < size {
        i *= 2;
    }
    i
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

    let mut first = false;

    struct Task<'a> {
        name: &'a str,
        stacksize: u32,
        requires: &'a IndexMap<String, u32>,
    }
    let mut tasks: Vec<Task> = toml
        .tasks
        .iter()
        .map(|(name, task)| Task {
            name,
            stacksize: task
                .stacksize
                .unwrap_or_else(|| toml.stacksize.unwrap()),
            requires: &task.requires,
        })
        .collect();
    tasks.push(Task {
        name: "kernel",
        stacksize: toml.stacksize.unwrap(),
        requires: &toml.kernel.requires,
    });

    let suggest = if toml.target.starts_with("thumbv7m")
        || toml.target.starts_with("thumbv7em")
    {
        armv7m_suggest
    } else if toml.target.starts_with("thumbv8m") {
        armv8m_suggest
    } else {
        panic!("Unknown target {}", toml.target);
    };

    let mut suggestions = Vec::new();
    for task in tasks {
        if !first {
            first = true;
        } else {
            println!();
        }
        let mut elf_name = dist_dir.clone();
        elf_name.push(task.name);
        let buffer = std::fs::read(elf_name)?;
        let elf = match Object::parse(&buffer)? {
            Object::Elf(elf) => elf,
            o => bail!("Invalid Object {:?}", o),
        };

        let mut memory_sizes = memories
            .keys()
            .map(|name| (name.as_str(), 0))
            .collect::<IndexMap<_, _>>();
        for phdr in &elf.program_headers {
            if let Some(region) = output_region(phdr.p_vaddr) {
                memory_sizes[region] += phdr.p_memsz;
            }
        }
        memory_sizes["ram"] += task.stacksize as u64;

        println!("{}", task.name);
        let mut my_suggestions = Vec::new();
        for name in memories.keys() {
            let used = memory_sizes[name.as_str()];
            if let Some(&size) = task.requires.get(name) {
                let percent = used * 100 / size as u64;
                println!(
                    "  {:<6} {: >5} bytes ({}%)",
                    format!("{}:", name),
                    used,
                    percent
                );
                let suggestion = suggest(used);
                if suggestion != size as u64 {
                    my_suggestions.push((name, size, suggestion));
                }
            } else {
                assert!(used == 0);
            }
        }
        if !my_suggestions.is_empty() {
            suggestions.push((task.name, my_suggestions));
        }
    }

    if !suggestions.is_empty() {
        let mut savings = memories
            .keys()
            .map(|name| (name.as_str(), 0))
            .collect::<IndexMap<_, _>>();
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
                savings[mem.as_str()] += prev as u64 - value;
            }
        }
        println!("\n------------ Total savings ------------");
        for (mem, savings) in &savings {
            println!("  {:<6} {}", format!("{}:", mem), savings);
        }
    }

    Ok(())
}
