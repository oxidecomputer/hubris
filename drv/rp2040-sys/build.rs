// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    idol::server::build_server_support(
        "../../idl/rp2040-sys.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}
