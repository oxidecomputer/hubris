// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Context;
use serde::Deserialize;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    trusted_keys: Option<Vec<PathBuf>>,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::build_notifications()?;
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/control-plane-agent.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    if let Some(tk) = build_util::task_maybe_config::<Config>()
        .context("could not parse config.control_plane_agent")?
        .and_then(|cfg| cfg.trusted_keys)
    {
        if tk.is_empty() {
            panic!("cannot provide empty set of trusted keys");
        }
        let out_dir = build_util::out_dir();
        let dest_path = out_dir.join("trusted_keys.rs");
        let mut out = std::fs::File::create(&dest_path).with_context(|| {
            format!("failed to create file '{}'", dest_path.display())
        })?;
        writeln!(&mut out, "const TRUSTED_KEYS: [[u8; 65]; {}] = [", tk.len())?;
        for k in tk {
            let key = ssh_key::PublicKey::read_openssh_file(&k)
                .with_context(|| format!("failed to read public key: {k:?}"))?;
            let pub_bytes = key
                .key_data()
                .ecdsa()
                .expect("must be ECDSA key")
                .as_sec1_bytes();
            writeln!(&mut out, "    {pub_bytes:?},")?;
        }
        writeln!(&mut out, "];")?;
    }

    Ok(())
}
