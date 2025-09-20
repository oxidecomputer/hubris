// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dist::PackageConfig;
use anyhow::{anyhow, bail, Context, Result};
use cargo_metadata::DependencyKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    io::Read,
    path::PathBuf,
};

#[derive(Debug, Deserialize, Clone)]
pub struct LspClient {
    toml: String,
    task: String,
}

impl std::str::FromStr for LspClient {
    type Err = serde_json::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

/// The dumbest subset of a `Cargo.toml` manifest, sufficient to get the name
#[derive(Deserialize)]
struct Manifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

/// Configuration to send to the text editor
#[derive(Serialize, Hash)]
#[serde(rename_all = "camelCase")]
struct LspConfig {
    target: String,
    features: Vec<String>,
    extra_env: BTreeMap<String, String>,
    hash: String,
    build_override_command: Vec<String>,
    app: String,
    task: String,
}

////////////////////////////////////////////////////////////////////////////////

struct PackageGraph(BTreeMap<String, cargo_metadata::Package>);

impl PackageGraph {
    fn new(metadata: cargo_metadata::Metadata) -> Self {
        let packages = metadata
            .packages
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect::<BTreeMap<_, _>>();
        Self(packages)
    }

    fn resolve(
        &self,
        root: &str,
        features: &[String],
    ) -> BTreeMap<String, BTreeSet<String>> {
        // We're going to calculate this package's dependencies, taking features
        // into account.  `package_dependencies` and `package_features` store
        // a mapping from package name to their enabled dependencies and
        // features respectively.
        let mut package_features: BTreeMap<String, BTreeSet<String>> =
            BTreeMap::new();
        let mut package_dependencies: BTreeMap<String, BTreeSet<String>> =
            BTreeMap::new();

        // We're going to build those mappings by starting at the root with a
        // known set of features enabled.
        let mut todo: Vec<(String, Option<String>)> = features
            .iter()
            .map(|f| (root.to_owned(), Some(f.clone())))
            .chain(std::iter::once((root.to_owned(), None)))
            .collect();

        // Ferris's tenth rule: "Any sufficiently complicated Rust build
        // system contains an ad hoc, informally-specified, bug-ridden, slow
        // implementation of half of Cargo."
        //
        // Dependency resolution and feature unification could be simple, but
        // for optional features of the form `crate?/feat`.  Optional
        // features may become active **after** they have already been
        // considered, which makes things trickier!
        //
        // For example, if we are enabling features "foo" and "baz"
        // ```toml
        // foo = ["bar?/lol"]
        // baz = ["bar"]
        // ```
        // when "foo" is checked, it does not know that "bar" is enabled.
        //
        // To work around this, we accumulate optional features in a separate
        // list (`optional`), then recheck them after all of the mandatory
        // features and dependencies have been handled.
        //
        // Once everything stabilizes, we know that any remaining optional
        // features did not get enabled, and we break out of the loop.
        loop {
            let mut changed = false;
            let mut optional = vec![];
            while let Some((pkg_name, feat)) = todo.pop() {
                // Anything not in `packages` is something from outside the
                // workspace, so we don't care about it.
                let Some(pkg) = self.0.get(&pkg_name) else {
                    continue;
                };

                // If we've never seen this package before, then insert all of
                // its non-optional dependencies with their features.
                if !package_features.contains_key(&pkg_name) {
                    assert!(!package_dependencies.contains_key(&pkg_name));

                    // Start with no features enabled
                    package_features.entry(pkg_name.clone()).or_default();
                    package_dependencies.entry(pkg_name.clone()).or_default();
                    changed = true;

                    // Insert all non-optional, non-build dependencies
                    for d in pkg.dependencies.iter() {
                        if !d.optional && d.kind != DependencyKind::Build {
                            // Record this dependency
                            changed |= package_dependencies
                                .get_mut(&pkg_name)
                                .unwrap()
                                .insert(d.name.clone());

                            // Queue up the dependent package for evaluation
                            todo.push((d.name.clone(), None));
                            if d.uses_default_features {
                                todo.push((
                                    d.name.clone(),
                                    Some("default".to_owned()),
                                ));
                            }
                            for f in &d.features {
                                todo.push((d.name.clone(), Some(f.to_owned())))
                            }
                        }
                    }
                }

                // Check to see if we're also enabling a feature here
                let Some(feat) = feat else { continue };

                if let Some(f) = pkg.features.get(&feat) {
                    // Queue up everything downstream of this feature for
                    // evaluation.
                    changed |= package_features
                        .get_mut(&pkg_name)
                        .unwrap()
                        .insert(feat);
                    todo.extend(
                        f.iter().map(|f| (pkg_name.clone(), Some(f.clone()))),
                    );
                } else if feat == "default" {
                    // Someone tried to enable the default features for this
                    // crate, but there are no default features; continue.
                    continue;
                } else {
                    let s = feat.strip_prefix("dep:").unwrap_or(&feat);
                    let (cra, fea) = if s.contains("?/") {
                        let mut iter = s.split("?/");
                        let cra = iter.next().unwrap();
                        let fea = iter.next().unwrap();
                        if package_dependencies[&pkg_name].contains(cra) {
                            (cra, Some(fea))
                        } else {
                            optional.push((pkg_name.clone(), Some(feat)));
                            continue;
                        }
                    } else if s.contains('/') {
                        let mut iter = s.split('/');
                        let cra = iter.next().unwrap();
                        let fea = iter.next().unwrap();
                        (cra, Some(fea))
                    } else {
                        (s, None)
                    };

                    changed |= package_dependencies
                        .entry(pkg_name)
                        .or_default()
                        .insert(cra.to_owned());

                    let d = pkg
                        .dependencies
                        .iter()
                        .find(|d| d.name == cra)
                        .unwrap();
                    if d.kind != DependencyKind::Build {
                        todo.push((cra.to_owned(), fea.map(|s| s.to_owned())))
                    }
                }
            }
            if !changed {
                break;
            }
            // Start the loop anew, checking whether the optional `crate?/feat`
            // are now active.
            assert!(todo.is_empty());
            todo = optional;
        }

        package_features
    }
}

/// Checks whether the given package is valid for the given task
fn check_task(
    package_name: &str,
    task_name: &str,
    app_name: &str,
    app_cfg: &PackageConfig,
    packages: &PackageGraph,
) -> Option<LspConfig> {
    let task = &app_cfg.toml.tasks[task_name];

    // Check to see if our target package is used in this task (resolved based
    // on per-task features)
    let dependencies = packages.resolve(&task.bin_crate, &task.features);

    // Congrats, we've found a task in the given image which uses our
    // desired crate.  Let's do some stuff with it.
    if dependencies.contains_key(package_name) {
        let build_cfg = app_cfg
            .toml
            .task_build_config(task_name, false, None)
            .map_err(|_| {
                anyhow!("could not get build config for {}", task_name)
            })
            .unwrap();

        let mut iter = build_cfg.args.iter();
        let mut target = None;
        while let Some(t) = iter.next() {
            if t == "--target" {
                iter.next().clone_into(&mut target);
            }
        }

        if target.is_none() {
            panic!("missing --target argument in build config");
        }

        let features: Vec<String> = dependencies
            .iter()
            .flat_map(|(package, feat)| {
                feat.iter().map(move |f| format!("{package}/{f}"))
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
        build_override_command
            .push(format!("--features={}", features.join(",")));

        let mut out = LspConfig {
            features,
            target: target.unwrap().to_string(),
            extra_env: build_cfg.env,
            hash: "".to_owned(),
            build_override_command,
            app: app_name.to_owned(),
            task: task_name.to_owned(),
        };

        let mut s = DefaultHasher::new();
        out.hash(&mut s);
        out.hash = format!("{:x}", s.finish());
        out.hash.truncate(8);

        return Some(out);
    }
    None
}

////////////////////////////////////////////////////////////////////////////////

fn inner(file: &PathBuf, clients: &[LspClient]) -> Result<LspConfig> {
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
    let cargo_toml: Manifest =
        toml::from_str(std::str::from_utf8(&cargo_text)?)?;
    let package_name = cargo_toml.package.name;

    let metadata = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .features(cargo_metadata::CargoOpt::AllFeatures)
        .exec()
        .context("failed to run cargo metadata")?;

    let root = metadata.workspace_root.clone();
    let packages = PackageGraph::new(metadata);

    // First, see if we have a matching client in the list of clients provided
    // to the CLI.  This minimizes the number of unique `rust-analyzer`
    // configurations running simultaneously, and speeds up initial attach time.
    for c in clients {
        // TODO: we parse the PackageConfig multiple times here, which may be
        // slow (but probably not slower than `cargo metadata` above)
        let file = root.join(&c.toml);
        let app_cfg = PackageConfig::new(&file, false, false)
            .context(format!("could not open {file:?}"))?;
        if let Some(out) =
            check_task(&package_name, &c.task, &c.toml, &app_cfg, &packages)
        {
            return Ok(out);
        }
    }

    let preferred_apps = if let Ok(toml) = std::env::var("HUBRIS_APP") {
        vec![toml]
    } else {
        vec![
            "app/gimlet/rev-c.toml".to_string(),
            "app/sidecar/rev-b.toml".to_string(),
            "app/psc/rev-b.toml".to_string(),
        ]
    };
    let preferred_task = std::env::var("HUBRIS_TASK").ok();
    for app_name in &preferred_apps {
        let file = root.join(app_name);
        let app_cfg = PackageConfig::new(&file, false, false)
            .context(format!("could not open {file:?}"))?;

        // See if we can find a valid task within this app_cfg
        if let Some(task_name) = &preferred_task {
            if let Some(lspconfig) = check_task(
                &package_name,
                task_name,
                app_name,
                &app_cfg,
                &packages,
            ) {
                return Ok(lspconfig);
            }
        } else if let Some(out) = app_cfg
            .toml
            .tasks
            .keys()
            .flat_map(|task_name| {
                check_task(
                    &package_name,
                    task_name,
                    app_name,
                    &app_cfg,
                    &packages,
                )
            })
            .next()
        {
            return Ok(out);
        }
    }

    // Try to be specific about the error condition.
    let apps = preferred_apps.join(", ");
    if let Some(taskname) = preferred_task {
        Err(anyhow!(
            "task {taskname} not found or {package_name} is not used; \
                checked apps: {apps}"
        ))
    } else {
        Err(anyhow!(
            "{package_name} is not used in checked apps: {apps}"
        ))
    }
}

pub fn run(file: &PathBuf, clients: &[LspClient]) -> Result<()> {
    let out = inner(file, clients).map_err(|e| e.to_string());
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}
