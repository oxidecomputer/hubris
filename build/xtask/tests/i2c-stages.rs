// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Snapshots of the `xtask i2c-codegen --stage ...` dumps of the load and
//! analysis stages for a small manifest. (The codegen stage is covered by
//! the `codegen-*` snapshots in `i2c-codegen.rs`.)

use std::path::Path;

use build_i2c::Disposition;
use insta::assert_snapshot;
use xtask::i2c_codegen::{Stage, run_stage};

#[test]
fn stage_dumps() {
    // Manifests only load from the base of the repository.
    std::env::set_current_dir(Path::new("../../")).unwrap();

    let manifest = Path::new("app/gimletlet/app-meanwell.toml");
    for stage in [Stage::Load, Stage::Analysis] {
        let out = run_stage(manifest, Disposition::Sensors, stage).unwrap();
        assert_snapshot!(format!("{stage:?}"), out);
    }
}
