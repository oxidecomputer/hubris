// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Checks that analysis succeeds for every application manifest in the tree.
//!
//! This is a much broader (if much shallower) check than the snapshots in
//! `i2c-codegen.rs`: it doesn't care what code we generate, only that every
//! manifest we ship can be analyzed without error.

use build_i2c::{AnalysisSettings, ControllerRole, analysis};
use std::path::{Path, PathBuf};

#[test]
fn every_manifest_analyzes() {
    // oh no, loading manifests doesn't work if we aren't at the base of the
    // repository.
    std::env::set_current_dir(Path::new("../../")).unwrap();

    let mut manifests = vec![];

    for dir in std::fs::read_dir("app").unwrap() {
        let dir = dir.unwrap().path();

        if !dir.is_dir() {
            continue;
        }

        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();

            if path.extension().is_some_and(|e| e == "toml") {
                manifests.push(path);
            }
        }
    }

    manifests.sort();
    assert!(!manifests.is_empty(), "found no manifests to check");

    let settings = AnalysisSettings {
        role: ControllerRole::Initiator,
        component_ids: false,
        // Computing the validation drivers has side effects on the build, so
        // it stays opt-in; we don't need it here.
        drivers: None,
        ..Default::default()
    };

    let mut checked: Vec<PathBuf> = vec![];
    let mut failures = vec![];

    for manifest in &manifests {
        //
        // Not every TOML file in `app/` is an application manifest (some are
        // included fragments); skip anything xtask itself won't load.
        //
        let Ok(config) = xtask::config::Config::from_file(manifest) else {
            continue;
        };

        let Some(config) = &config.config else {
            continue;
        };

        let toml = toml::to_string(config).unwrap();
        let value: toml::Value = toml::from_str(&toml).unwrap();

        if value.get("i2c").is_none() {
            continue;
        }

        let config = match xtask::i2c_codegen::load_config(manifest) {
            Ok(config) => config,
            Err(e) => {
                failures.push(format!(
                    "{}: load failed: {e:#}",
                    manifest.display()
                ));
                continue;
            }
        };

        match analysis::analyze(config, &settings) {
            Ok(_) => checked.push(manifest.clone()),
            Err(e) => {
                failures.push(format!("{}: {e:#}", manifest.display()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "analysis failed for {} manifest(s):\n{}",
        failures.len(),
        failures.join("\n")
    );

    eprintln!(
        "analyzed {} of {} manifests",
        checked.len(),
        manifests.len()
    );

    assert!(
        checked.len() > 10,
        "expected to check more manifests than {}",
        checked.len()
    );
}
