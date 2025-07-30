// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::build_notifications()?;
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/stm32h7-update.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    let out = build_util::out_dir();
    let mut ver_file = File::create(out.join("consts.rs")).unwrap();

    let version: u32 = build_util::env_var("HUBRIS_BUILD_VERSION")?.parse()?;
    let epoch: u32 = build_util::env_var("HUBRIS_BUILD_EPOCH")?.parse()?;

    writeln!(ver_file, "const HUBRIS_BUILD_VERSION: u32 = {version};")?;
    writeln!(ver_file, "const HUBRIS_BUILD_EPOCH: u32 = {epoch};")?;

    Ok(())
}
