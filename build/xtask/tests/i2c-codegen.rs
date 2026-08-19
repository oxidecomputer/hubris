// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Okay, so this is actually testing `build/i2c`, and the test for this should
//! probably live there, but right now the easiest way to invoke code generation
//! is by using the xtask implementation. We can't depend on xtask from
//! build-i2c because that would lead to circular dependencies.
//!
//! So, pragmatically, xtask is going to host the snapshot testing party for
//! build-i2c to avoid churning a lot of other things.
//!
//! ALSO, note that the existence of this type is intended to be a temporary
//! band-aid for a lack of any testing for `build-i2c`. The hope is to someday
//! test this more directly using unit tests of analysis and much more targeted
//! snapshot testing of generation behavior, which will allow for much smaller
//! fragments and less breakage on intentional changes.
//!
//! Apologies to those that need to update these snapshots until that day comes.
//! You will need to install `cargo-insta`, e.g. `cargo install cargo-insta`,
//! run the tests with `cargo insta test -p xtask`, and then review+bless any
//! new changes with `cargo insta review`.

use std::path::Path;

use anyhow::Result;
use build_i2c::{ConfigGenerator, Disposition};
use insta::assert_snapshot;
use tempfile::tempdir;

#[test]
fn snapshot() {
    let manifests: &[&Path] = &[
        Path::new("app/gimlet/rev-f-dev.toml"),
        Path::new("app/cosmo/rev-b-dev.toml"),
        Path::new("app/sidecar/rev-d-dev.toml"),
        Path::new("app/observer/rev-a-dev.toml"),
        Path::new("app/psc/rev-c-dev.toml"),
    ];
    type GenFn = fn(&ConfigGenerator, &mut String) -> Result<()>;

    // TODO: Some analysis and generation is gated in either `new_with_config`
    // or in the generate functions themselves to only work with certain
    // dispositions. We should probably reconsider this at some point, and make
    // snapshotting just per-function and not require a manual statement of
    // disposition here.
    let funcs: &[(&str, Disposition, GenFn)] = &[
        (
            "controllers",
            Disposition::Initiator,
            ConfigGenerator::generate_controllers,
        ),
        (
            "devices",
            Disposition::Sensors,
            ConfigGenerator::generate_devices,
        ),
        (
            "muxes",
            Disposition::Sensors,
            ConfigGenerator::generate_muxes,
        ),
        (
            "pins",
            Disposition::Initiator,
            ConfigGenerator::generate_pins,
        ),
        (
            "ports",
            Disposition::Sensors,
            ConfigGenerator::generate_ports,
        ),
        (
            "validation",
            Disposition::Validation,
            ConfigGenerator::generate_validation,
        ),
    ];

    // oh no, loading manifests doesn't work if we aren't at the base of the
    // repository.
    std::env::set_current_dir(Path::new("../../")).unwrap();

    // temporary directory so we can invoke rustfmt on the snapshots
    let tempdir = tempdir().unwrap();

    for manifest in manifests {
        for (case, disp, f) in funcs {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.{case}-{disp:?}");
            let dest = format!("{name}.snap");
            let temp_out = tempdir.path().join(Path::new(&dest));

            let mut out = String::new();

            // Create the generator...
            let g =
                xtask::i2c_codegen::setup_generator(*manifest, (*disp).into())
                    .unwrap();
            // Do code generation with the given function
            (f)(&g, &mut out).unwrap();
            // Write and format the file...
            xtask::i2c_codegen::write_file(&out, &temp_out, true).unwrap();

            // ...then read it back
            let contents = std::fs::read_to_string(temp_out).unwrap();
            assert_snapshot!(name, contents);
        }
    }
}
