// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::client::build_client_stub(
        "../../idl/transceivers.idol",
        "client_stub.rs",
    )?;

    let disposition = build_i2c::Disposition::Sensors;
    if let Err(e) = build_i2c::codegen(disposition) {
        println!("cargo::error=code generation failed: {e}");
        std::process::exit(1);
    }

    Ok(())
}
