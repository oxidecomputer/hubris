// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

struct Manifest {
    package: Package,
}
struct Package {
    name: String,
}

pub fn run(file: &PathBuf, env: bool) -> Result<()> {
    if !file.is_file() {
        bail!("input must be a file");
    }
    let mut dir = file
        .parent()
        .ok_or_else(|| anyhow!("could not find parent of {file:?}"))?;

    let cargo = loop {
        println!("checking {dir:?}");
        if let Ok(f) = std::fs::File::open(dir.join("Cargo.toml")) {
            break f;
        }
        dir = dir
            .parent()
            .ok_or_else(|| anyhow!("reached root of filesystem"))?;
    };
    println!("Found cargo: {cargo:?}");

    Ok(())
}
