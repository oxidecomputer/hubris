// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result, bail};
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
    build_i2c::codegen(build_i2c::Disposition::Devices).inspect_err(|e| {
        println!("cargo::error=failed to generate I2C devices: {e}");
    })?;

    do_pmbus()?;

    Ok(())
}

fn do_pmbus() -> Result<()> {
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("pmbus_mapping.rs");
    let out = context_create_file(&dest_path)?;
    let mut file = std::io::BufWriter::new(out);

    let mut pmbus_rails = std::collections::BTreeMap::new();
    let mut pmbus_rail_dupes = 0;
    for dev in build_i2c::device_descriptions() {
        // We only need to map PMBus devices
        let Some(ref pmbus) = dev.pmbus else {
            continue;
        };

        // Aggregate a list of all PMBus-visible rails
        let multi_rail = pmbus.rails.len() != 1;
        for rail in pmbus.rails.iter() {
            // `BTreeSet::insert` return value means "is unique", which is the
            // inverse of `BTreeMap::insert().is_some()`!
            if pmbus_rails.insert(rail.name.clone(), multi_rail).is_some() {
                pmbus_rail_dupes += 1;
                print!("cargo::error=PMBus device ");
                print!(
                    "{} defines a power rail ",
                    dev.device_id.as_deref().unwrap_or("(no ID)")
                );
                println!(
                    "{:?} which already exists in the manifest",
                    rail.name
                );
            }
        }
    }

    // Create a mapping between rail names and generated accessor functions for
    // obtaining the device handle and rail index
    writeln!(file)?;
    writeln!(
        file,
        "pub const PMBUS_RAIL_TO_I2C_DEVICE_MAP: [PmbusRailBinding; {}] = [",
        pmbus_rails.len()
    )?;
    for (rail, is_multi) in pmbus_rails.iter() {
        write!(file, "    PmbusRailBinding {{ ")?;
        write!(file, "name: \"{rail}\", ")?;
        // build_i2c *also* only to-lowercases the rail names to make functions
        write!(
            file,
            "summon_fn: crate::i2c_config::pmbus::{}, ",
            rail.to_lowercase()
        )?;
        write!(file, "multi_rail: {is_multi} ")?;
        writeln!(file, "}},")?;
    }
    writeln!(file, "];")?;

    // This is supposed to be caught during I2C generation
    if pmbus_rail_dupes != 0 {
        bail!("duplicate PMBus rails: invalid application toml.");
    }

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
