// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() {
    build_util::expose_target_board();

    let disposition = build_i2c::Disposition::Initiator;

    #[cfg(feature = "standalone")]
    let artifact = build_i2c::Artifact::Standalone;

    #[cfg(not(feature = "standalone"))]
    let artifact = build_i2c::Artifact::Dist;

    if let Err(e) = build_i2c::codegen(disposition, artifact) {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }
}
