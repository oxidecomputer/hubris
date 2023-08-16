// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::server::build_server_support(
        "../../idl/packrat.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    // Ensure the "gimlet" feature is enabled on gimlet boards.
    #[cfg(not(feature = "gimlet"))]
    match build_util::target_board().as_deref() {
        Some("gimlet-b" | "gimlet-c" | "gimlet-d" | "gimlet-e") => {
            panic!(concat!(
                "packrat's `gimlet` feature should be enabled when ",
                "building for gimlets",
            ))
        }
        _ => (),
    }

    // Ensure the "gimlet" feature is _not_ enabled on sidecar/psc boards.
    #[cfg(feature = "gimlet")]
    match build_util::target_board().as_deref() {
        Some("psc-a" | "psc-b" | "psc-c") => panic!(concat!(
            "packrat's `gimlet` feature should not be enabled when ",
            "building for PSCs",
        )),
        Some("sidecar-b" | "sidecar-c") => panic!(concat!(
            "packrat's `gimlet` feature should not be enabled when ",
            "building for sidecars",
        )),
        _ => (),
    }

    Ok(())
}
