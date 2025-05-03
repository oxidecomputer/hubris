// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::Generator::new()
        .with_counters(idol::CounterSettings::default())
        .build_client_stub(
            "../../idl/spartan7-loader.idol",
            "client_stub.rs",
        )?;
    Ok(())
}
