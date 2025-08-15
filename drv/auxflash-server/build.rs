// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::build_notifications()?;
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/auxflash.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    match build_util::env_var("HUBRIS_AUXFLASH_CHECKSUM") {
        Ok(e) => {
            let out_dir = build_util::out_dir();
            let dest_path = out_dir.join("checksum.rs");
            let mut file = std::fs::File::create(dest_path)?;
            writeln!(&mut file, "const AUXI_CHECKSUM: [u8; 32] = {e};")?;
        }
        Err(e) => panic!(
            "Could not find HUBRIS_AUXFLASH_CHECKSUM in environment. \
                    Is there at least one [[auxflash.blobs]] in the app?\n\
            {e:?}",
        ),
    }

    Ok(())
}
