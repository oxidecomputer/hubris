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

use std::path::Path;

use build_i2c::Disposition;
use insta::assert_snapshot;
use tempfile::tempdir;

#[test]
fn snapshot() {
    let disps: &[Disposition] = &[Disposition::Sensors];
    let manifests: &[&Path] = &[
        Path::new("app/gimlet/rev-f-dev.toml"),
        Path::new("app/cosmo/rev-b-dev.toml"),
        Path::new("app/sidecar/rev-d-dev.toml"),
        Path::new("app/observer/rev-a-dev.toml"),
        Path::new("app/psc/rev-c-dev.toml"),
    ];

    // oh no, loading manifests doesn't work if we aren't at the base of the
    // repository.
    std::env::set_current_dir(Path::new("../../")).unwrap();

    // temporary directory so we can invoke rustfmt on the snapshots
    let tempdir = tempdir().unwrap();

    for manifest in manifests {
        for disp in disps {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.{disp:?}");
            let dest = format!("{name}.snap");
            let temp_out = tempdir.path().join(Path::new(&dest));

            // Write and format the file...
            xtask::i2c_codegen::run(*manifest, *disp, Some(&temp_out), true)
                .unwrap();

            // ...then read it back
            let contents = std::fs::read_to_string(temp_out).unwrap();
            assert_snapshot!(name, contents);
        }
    }
}
