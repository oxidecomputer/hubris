// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `cargo xtask clean-lite`: remove firmware build products, but keep host
//! artifacts (`xtask` itself, build scripts, and proc macros)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Returns everything that `xtask clean-lite` should remove
fn find_targets(target: &Path) -> Result<Vec<PathBuf>> {
    // Each of these is laid out like a Cargo target directory. `target/`
    // itself is also where `dist` puts linker scripts and per-app output.
    let mut work_dirs = vec![target.to_owned()];
    for entry in read_dir(&target.join("bindeps"))? {
        work_dirs.push(entry.path());
    }

    let mut out = vec![];
    for dir in &work_dirs {
        for entry in read_dir(dir)? {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Filter to `*-none-*` triples as a reasonable way to avoid host
            // built artifacts.
            let is_firmware_triple = path.is_dir() && name.contains("-none-");
            let is_link_script = matches!(&*name, "link.x" | "memory.x");
            if is_firmware_triple || is_link_script {
                out.push(path);
            }
        }
    }

    // `xtask dist $APP_NAME` output lives in `target/$APP_NAME/dist`
    for entry in read_dir(target)? {
        let dist = entry.path().join("dist");
        if dist.is_dir() {
            out.push(entry.path());
        }
    }

    out.sort();
    Ok(out)
}

/// Lists a directory, treating a missing directory as empty
fn read_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .collect::<Result<_, _>>()
            .with_context(|| format!("could not read {}", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => {
            Err(e).with_context(|| format!("could not read {}", dir.display()))
        }
    }
}

pub fn run(dry_run: bool) -> Result<()> {
    let targets = find_targets(Path::new("target"))?;
    if targets.is_empty() {
        println!("nothing to clean");
        return Ok(());
    }
    for path in targets {
        if dry_run {
            println!("would remove {}", path.display());
            continue;
        }
        println!("removing {}", path.display());
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result
            .with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(())
}
