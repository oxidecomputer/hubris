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
    app: String,
    task: String,
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
        .features(cargo_metadata::CargoOpt::AllFeatures)
        .exec()
        .context("failed to run cargo metadata")?;

    let packages = metadata
        .packages
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect::<BTreeMap<_, _>>();

    let root = metadata.workspace_root;

    let preferred_apps = [
        "app/gimlet/rev-c.toml",
        "app/sidecar/rev-b.toml",
        "app/psc/rev-b.toml",
    ];
    for p in preferred_apps {
        let file = root.join(p);
        let cfg = PackageConfig::new(&file, false, false)
            .context(format!("could not open {file:?}"))?;

        for (name, task) in &cfg.toml.tasks {
            // We're going to calculate this package's dependencies, taking
            // features into account.  The `dependencies` array below stores a
            // mapping from packages to their enabled features; we build it by
            // recursing down the whole package tree, starting at the task.
            let mut dependencies: BTreeMap<String, BTreeSet<String>> =
                BTreeMap::new();
            let mut todo: Vec<(String, bool, BTreeSet<String>)> = vec![(
                task.name.clone(),
                false,
                task.features.clone().into_iter().collect(),
            )];

            // Ferris's tenth rule: "Any sufficiently complicated Rust build
            // system contains an ad hoc, informally-specified, bug-ridden, slow
            // implementation of half of Cargo."
            while let Some((pkg_name, default_feat, mut feat)) = todo.pop() {
                let pkg = match packages.get(&pkg_name) {
                    Some(pkg) => pkg,
                    None => continue,
                };

                let package_features: BTreeSet<String> =
                    pkg.features.keys().cloned().collect();

                // Features can enable other features; calculate the full set of
                // features by iterating over them repeatedly until the set of
                // enabled features stabilizes.
                loop {
                    let mut changed = false;
                    for (f, sub) in &pkg.features {
                        if feat.contains(f) || (f == "default" && default_feat)
                        {
                            for s in sub.iter() {
                                if package_features.contains(s) {
                                    changed |= feat.insert(s.to_string());
                                }
                            }
                        }
                    }
                    if !changed {
                        break;
                    }
                }

                // Feature unification: if we've already seen this package, and
                // the currently-enabled features don't add anything new, then
                // we can skip it.
                {
                    let mut changed = !dependencies.contains_key(&pkg_name);
                    let entry =
                        dependencies.entry(pkg_name.clone()).or_default();
                    for f in feat.iter() {
                        changed |= entry.insert(f.to_string());
                    }
                    if !changed {
                        continue;
                    }
                }

                // Store the crates to examine next
                let mut next: BTreeMap<String, _> = pkg
                    .dependencies
                    .iter()
                    .map(|d| (d.name.clone(), d.clone()))
                    .collect();

                // Now that we've got features, we can figure out which
                // dependencies are enabled by features.  We iterate over
                // feature that is enabled, examining anything that *it* enables
                // which is not itself a feature (since those were handled
                // above).  This means that everything handled in this loop is
                // a dependency of some work ('dep', 'dep/feat', 'dep?/feat')
                for s in pkg
                    .features
                    .iter()
                    .filter(|f| feat.contains(f.0))
                    .flat_map(|(_, sub)| sub.iter())
                    .filter(|f| !package_features.contains(*f))
                {
                    if s.contains("?/") {
                        let mut iter = s.split("?/");
                        let cra = iter.next().unwrap();
                        let fea = iter.next().unwrap();
                        next.get_mut(cra)
                            .unwrap()
                            .features
                            .push(fea.to_owned());
                    } else if s.contains('/') {
                        let mut iter = s.split('/');
                        let cra = iter.next().unwrap();
                        let fea = iter.next().unwrap();
                        let t = next.get_mut(cra).unwrap();
                        t.optional = false;
                        t.features.push(fea.to_owned());
                    } else if let Some(s) = s.strip_prefix("dep:") {
                        next.get_mut(s).unwrap().optional = false;
                    } else {
                        next.get_mut(s).unwrap().optional = false;
                    }
                }
                for n in next.values() {
                    if n.kind != cargo_metadata::DependencyKind::Build
                        && !n.optional
                    {
                        todo.push((
                            n.name.clone(),
                            n.uses_default_features,
                            n.features.iter().cloned().collect(),
                        ));
                    }
                }
            }

            // Congrats, we've found a task in the given image which uses our
            // desired crate.  Let's do some stuff with it.
            if dependencies.contains_key(&package_name) {
                let build_cfg =
                    cfg.toml.task_build_config(&name, false, None).map_err(
                        |_| anyhow!("could not get build config for {}", name),
                    )?;

                let mut iter = build_cfg.args.iter();
                let mut target = None;
                while let Some(t) = iter.next() {
                    if t == "--target" {
                        target = iter.next().to_owned();
                    }
                }

                if target.is_none() {
                    bail!("missing --target argument");
                }

                let features: Vec<String> = dependencies
                    .iter()
                    .flat_map(|(package, feat)| {
                        feat.iter().map(move |f| format!("{package}/{f}"))
                    })
                    .collect();

                let exclude_dirs: Vec<String> = packages
                    .values()
                    .filter(|p| !dependencies.contains_key(&p.name))
                    .map(|p| {
                        pathdiff::diff_paths(
                            p.manifest_path.parent().unwrap(),
                            &root,
                        )
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned()
                    })
                    .collect();

                let mut build_override_command: Vec<String> =
                    "cargo check --message-format=json"
                        .split(' ')
                        .map(|s| s.to_string())
                        .collect();
                build_override_command.extend(build_cfg.args.iter().cloned());
                build_override_command
                    .extend(dependencies.keys().map(|p| format!("-p{p}")));
                build_override_command.push("--target-dir".to_owned());
                build_override_command.push("target/rust-analyzer".to_owned());
                build_override_command
                    .push(format!("--features={}", features.join(",")));

                let mut out = LspConfig {
                    features,
                    target: target.unwrap().to_string(),
                    extra_env: build_cfg.env,
                    hash: "".to_owned(),
                    exclude_dirs,
                    build_override_command,
                    app: p.clone().to_owned(),
                    task: name.clone(),
                };

                let mut s = DefaultHasher::new();
                out.hash(&mut s);
                out.hash = format!("{:x}", s.finish());
                out.hash.truncate(8);

                return Ok(out);
            }
        }
    }
    Err(anyhow!("could not find {package_name} used in any apps"))
}

pub fn run(file: &PathBuf, env: bool) -> Result<()> {
    let out = inner(file, env).map_err(|e| e.to_string());
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
