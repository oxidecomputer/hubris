// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dist::PackageConfig;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    io::Read,
    path::PathBuf,
};

/// The dumbest subset of a `Cargo.toml` manifest, sufficient to get the name
#[derive(Deserialize)]
struct Manifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

#[derive(Serialize, Hash)]
#[serde(rename_all = "camelCase")]
struct LspConfig {
    target: String,
    features: Vec<String>,
    extra_env: BTreeMap<String, String>,
    hash: String,
    exclude_dirs: Vec<String>,
    build_override_command: Vec<String>,
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

    let packages = metadata
        .packages
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect::<BTreeMap<_, _>>();
    let package = packages.get(&package_name).ok_or_else(|| {
        anyhow!("cannot find package {package_name} in cargo metadata")
    })?;

    // If this is a binary file, then we'll assume it's a task
    let is_bin = package
        .targets
        .iter()
        .any(|t| t.kind.iter().any(|k| k == "bin"));

    // TODO: handle build.rs files, which need the appropriate environmental
    // variables but don't build for the ARM target (?)

    // TODO: handle libraries
    if !is_bin {
        bail!("must run on task binaries");
    }

    let mut todo = vec![package.name.clone()];
    let mut dependencies = BTreeSet::new();
    while let Some(t) = todo.pop() {
        if packages.contains_key(&t) && dependencies.insert(t.clone()) {
            todo.extend(
                packages[&t]
                    .dependencies
                    .iter()
                    .filter(|s| s.kind != cargo_metadata::DependencyKind::Build)
                    .map(|s| s.name.clone())
                    .filter(|d| !dependencies.contains(d)),
            );
        }
    }

    let root = metadata.workspace_root;
    let exclude_dirs: Vec<String> = packages
        .values()
        .filter(|p| !dependencies.contains(&p.name))
        .map(|p| {
            pathdiff::diff_paths(p.manifest_path.parent().unwrap(), &root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();

    let preferred_apps = [
        "app/demo-stm32g0-nucleo/app-g031-mini.toml",
        "app/gimlet/rev-c.toml",
        "app/sidecar/rev-b.toml",
        "app/psc/rev-b.toml",
    ];
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

            let mut build_override_command: Vec<String> =
                "cargo check --message-format=json"
                    .split(' ')
                    .map(|s| s.to_string())
                    .collect();
            build_override_command.extend(build_cfg.args.iter().cloned());
            build_override_command.push(format!("-p{package_name}"));
            build_override_command.push("--target-dir".to_owned());
            build_override_command.push("target/rust-analyzer".to_owned());

            let mut out = LspConfig {
                features: features
                    .unwrap_or(&"".to_owned())
                    .split(',')
                    .map(|s| s.to_string())
                    .collect(),
                target: target.unwrap().to_string(),
                extra_env: build_cfg.env,
                hash: "".to_owned(),
                exclude_dirs,
                build_override_command,
            };

            let mut s = DefaultHasher::new();
            out.hash(&mut s);
            out.hash = format!("{:x}", s.finish());
            out.hash.truncate(8);

            return Ok(out);
        }
    }
    Err(anyhow!("could not find {package_name} used in any apps"))
}

pub fn run(file: &PathBuf, env: bool) -> Result<()> {
    let out = inner(file, env).map_err(|e| e.to_string());
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
