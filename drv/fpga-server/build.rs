// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();

    if cfg!(feature = "front_io") {
        let disposition = build_i2c::Disposition::Devices;

        if let Err(e) = build_i2c::codegen(disposition) {
            println!("code generation failed: {}", e);
            std::process::exit(1);
        }
    }

    idol::server::build_server_support(
        "../../idl/fpga.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}
