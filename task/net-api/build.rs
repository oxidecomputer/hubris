// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::client::build_client_stub("../../idl/net.idol", "client_stub.rs")?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("net_config.rs");
    let net_config = build_net::load_net_config()?;

    let mut out = std::fs::File::create(dest_path)?;

    if build_util::has_feature("vlan") {
        build_net::generate_vlan_consts(&net_config, &mut out)?;
    }

    build_net::generate_socket_enum(&net_config, &mut out)?;
    Ok(())
}
