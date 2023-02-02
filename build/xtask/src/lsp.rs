// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dist::PackageConfig;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

/// The dumbest subset of a `Cargo.toml` manifest, sufficient to get the name
#[derive(Deserialize)]
struct Manifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

#[derive(Serialize)]
struct LspConfig {
    target: String,
    features: Vec<String>,
    env: BTreeMap<String, String>,
}

fn inner(file: &PathBuf, _env: bool) -> Result<LspConfig> {
    if !file.is_file() {
        bail!("input must be a file");
    }
    let mut dir = file
        .parent()
        .ok_or_else(|| anyhow!("could not find parent of {file:?}"))?;

    let mut cargo = loop {
        if let Ok(f) = std::fs::File::open(dir.join("Cargo.toml")) {
            break f;
        }
        dir = dir
            .parent()
            .ok_or_else(|| anyhow!("reached root of filesystem"))?;
    };

    // Read the Cargo.toml to get the package name
    let mut cargo_text = Vec::new();
    cargo
        .read_to_end(&mut cargo_text)
        .context("failed to read Cargo.toml")?;
    let cargo_toml: Manifest = toml::from_slice(&cargo_text)?;
    let package_name = cargo_toml.package.name;

    let metadata = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to run cargo metadata")?;

    let package = metadata
        .packages
        .iter()
        .find(|p| p.name == package_name)
        .ok_or_else(|| {
            anyhow!("cannot find package {package_name} in cargo metadata")
        })?;

    // If this is a binary file, then we'll assume it's a task
    let is_bin = package
        .targets
        .iter()
        .any(|t| t.kind.iter().any(|k| k == "bin"));

    // TODO: handle build.rs files, which need the appropriate environmental
    // variables but don't build for the ARM target

    // TODO: handle libraries (YOLO)
    if !is_bin {
        bail!("must run on task binaries");
    }

    let preferred_apps = [
        "app/gimlet/rev-c.toml",
        "app/sidecar/rev-b.toml",
        "app/psc/rev-b.toml",
    ];
    let root = metadata.workspace_root;
    for p in preferred_apps {
        let file = root.join(p);
        let cfg = PackageConfig::new(&file, false, false)
            .context(format!("could not open {file:?}"))?;
        if let Some(t) = cfg
            .toml
            .tasks
            .iter()
            .find(|(_name, task)| task.name == package_name)
        {
            let build_cfg =
                cfg.toml.task_build_config(t.0, false, None).map_err(|_| {
                    anyhow!("could not get build config for {}", t.0)
                })?;

            let mut iter = build_cfg.args.iter();
            let mut features = None;
            let mut target = None;
            while let Some(t) = iter.next() {
                if t == "--features" {
                    features = iter.next().to_owned();
                }
                if t == "--target" {
                    target = iter.next().to_owned();
                }
            }

            if target.is_none() {
                bail!("missing --target argument");
            }

            return Ok(LspConfig {
                features: features
                    .unwrap_or(&"".to_owned())
                    .split(',')
                    .map(|s| s.to_string())
                    .collect(),
                target: target.unwrap().to_string(),
                env: build_cfg.env,
            });
        }
    }
    Err(anyhow!("could not find {package_name} used in any apps"))
}

pub fn run(file: &PathBuf, env: bool) -> Result<()> {
    let out = inner(file, env).map_err(|e| e.to_string());
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
