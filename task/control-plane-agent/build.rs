// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    /// List of public keys in OpenSSH format
    #[serde(default)]
    trusted_keys: Vec<PathBuf>,
    /// Single file in OpenSSH's `authorized_keys` format
    authorized_keys: Option<PathBuf>,
}

fn main() -> Result<()> {
    build_util::build_notifications()?;
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/control-plane-agent.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )
        .map_err(anyhow::Error::from_boxed)?;

    let cfg = build_util::task_maybe_config::<Config>()
        .context("could not parse config.control_plane_agent")?;

    if let Some(cfg) = cfg {
        write_keys(cfg)?;
    }

    // Generate the necessary rail names
    let i2c = build_i2c::codegen_to_file(build_i2c::Disposition::Devices)
        .inspect_err(|e| {
            println!("cargo::error=failed to generate I2C devices: {e}");
        })?;

    do_pmbus(&i2c.report)?;

    Ok(())
}

fn do_pmbus(report: &build_i2c::Report) -> Result<()> {
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("pmbus_mapping.rs");
    let out = context_create_file(&dest_path)?;
    let mut file = std::io::BufWriter::new(out);

    //
    // Every named rail on a PMBus device, in rail-name order. build-i2c has
    // already resolved each rail to its device and (for multi-rail devices)
    // its rail index, and rejected duplicate rail names.
    //
    // The device indices use the ordering shared by `device_descriptions()`
    // and the generated device lookup in `build_i2c`.
    //
    let rails: Vec<_> = report
        .rails
        .pmbus
        .iter()
        .filter(|rail| report.devices[rail.device].pmbus.is_some())
        .collect();

    writeln!(file)?;
    writeln!(
        file,
        "pub const PMBUS_RAIL_TO_I2C_DEVICE_MAP: [PmbusRailBinding; {}] = [",
        rails.len()
    )?;
    for rail in rails {
        let rail_index =
            rail.bank.map(u8::try_from).transpose().with_context(|| {
                format!(
                    "PMBus device {:?} has more than 256 rails",
                    report.devices[rail.device].config.refdes,
                )
            })?;
        writeln!(
            file,
            "    PmbusRailBinding {{ name: \"{}\", device_index: \
             {}, rail_index: {rail_index:?} }},",
            rail.rail, rail.device,
        )?;
    }
    writeln!(file, "];")?;

    Ok(())
}

fn write_keys(cfg: Config) -> Result<()> {
    if cfg.trusted_keys.is_empty() && cfg.authorized_keys.is_none() {
        panic!("must provide trusted-keys or authorized-keys");
    }

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("trusted_keys.rs");
    let mut out = context_create_file(&dest_path)?;

    let mut keys = vec![];
    for k in cfg.trusted_keys {
        println!("cargo:rerun-if-changed={}", k.display());
        let key = ssh_key::PublicKey::read_openssh_file(&k)
            .with_context(|| format!("failed to read public key: {k:?}"))?;
        let pub_bytes = key
            .key_data()
            .ecdsa()
            .expect("must be ECDSA key")
            .as_sec1_bytes();
        keys.push(format!("{pub_bytes:?}"));
    }
    if let Some(k) = cfg.authorized_keys {
        println!("cargo:rerun-if-changed={}", k.display());
        let ks = ssh_key::AuthorizedKeys::read_file(&k).with_context(|| {
            format!("failed to read authorized keys from: {k:?}")
        })?;
        for key in ks {
            let pub_bytes = key
                .public_key()
                .key_data()
                .ecdsa()
                .expect("must be ECDSA key")
                .as_sec1_bytes();
            keys.push(format!("{pub_bytes:?}"));
        }
    }

    writeln!(
        &mut out,
        "const TRUSTED_KEYS: [[u8; 65]; {}] = [",
        keys.len()
    )?;
    for k in keys {
        writeln!(&mut out, "    {k},")?;
    }
    writeln!(&mut out, "];")?;
    Ok(())
}

/// Create a file with anyhow context
fn context_create_file(path: &Path) -> Result<File> {
    File::create(path)
        .with_context(|| format!("failed to create file '{}'", path.display()))
}
