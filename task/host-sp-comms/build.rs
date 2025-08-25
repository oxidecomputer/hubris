// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    build_util::build_notifications()?;

    #[cfg(any(feature = "gimlet", feature = "cosmo"))]
    build_i2c::codegen(build_i2c::CodegenSettings {
        disposition: build_i2c::Disposition::Sensors,
        component_ids: true,
    })?;

    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/host-sp-comms.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    Ok(())
}
