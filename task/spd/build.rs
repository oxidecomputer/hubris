// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() {
    idol::server::build_server_support(
        "../../idl/spd.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )
    .unwrap();

    build_util::expose_target_board();

    let disposition = build_i2c::Disposition::Target;

    if let Err(e) = build_i2c::codegen(disposition) {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }
}
