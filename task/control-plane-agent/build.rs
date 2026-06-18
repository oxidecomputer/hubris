// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
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

    // Build a mapping from "pmbus device name" to "supported status regs"
    let pmbus_status_caps = {
        let mut map = HashMap::new();
        for (name, func) in PMBUS_GENERATOR {
            map.insert(*name, (func)());
        }
        map
    };

    let mut pmbus_rail_names = std::collections::BTreeMap::new();
    let mut pmbus_rail_dupes = 0;
    for dev in build_i2c::device_descriptions() {
        // We only need to map PMBus devices
        let Some(ref pmbus) = dev.pmbus else {
            continue;
        };

        // If it is a pmbus device, we need to get its status capabilities
        let Some(caps) = pmbus_status_caps.get(dev.device.as_str()) else {
            println!(
                "cargo::error=unknown pmbus device: {}, add entry to \
                 PMBUS_GENERATOR in {} for status register support.",
                dev.device,
                file!(),
            );
            panic!("Unsupported pmbus device: {}", dev.device);
        };

        // Aggregate a list of all PMBus-visible rails
        for rail in pmbus.rails.iter() {
            if pmbus_rail_names.insert(rail.name.clone(), caps).is_some() {
                pmbus_rail_dupes += 1;
                println!(
                    "cargo::error=PMBus device {} defines a power rail {:?} \
                     which already exists in the manifest",
                    dev.device_id.as_deref().unwrap_or("(no ID)"),
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
        pmbus_rail_names.len()
    )?;
    for (rail, caps) in pmbus_rail_names.iter() {
        write!(file, "    PmbusRailBinding {{ ")?;
        write!(file, "name: \"{rail}\", ")?;
        // build_i2c *also* only to-lowercases the rail names to make functions
        write!(
            file,
            "summon_fn: crate::i2c_config::pmbus::{}_with_opt_page_idx, ",
            rail.to_lowercase()
        )?;
        write!(file, "status_bits: Capabilities(0x{:08x}) ", caps.0)?;
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

/// Look at the `pmbus` crate metadata to see if a specific command is "Illegal"
/// and set the capability bit if not.
macro_rules! set_if_pmbus_read_illegal {
    ($out:ident, $module:ident, $cmd:ident) => {{
        use drv_i2c_types::pmbus_status::Capabilities;
        use pmbus::{Command, Operation};
        if pmbus::commands::$module::CommandCode::$cmd.read_op()
            != Operation::Illegal
        {
            $out |= Capabilities::$cmd.0;
        }
    }};
}

/// For a given device, calculate the `Capabilities` for each of the
/// status registers.
///
/// The pmbus functions are not const, so generate a closure instead.
macro_rules! generator {
    ($name:literal, $module:ident) => {
        ($name, || {
            let mut out = 0u32;
            set_if_pmbus_read_illegal!(out, $module, STATUS_WORD);
            set_if_pmbus_read_illegal!(out, $module, STATUS_VOUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_IOUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_TEMPERATURE);
            set_if_pmbus_read_illegal!(out, $module, STATUS_CML);
            set_if_pmbus_read_illegal!(out, $module, STATUS_OTHER);
            set_if_pmbus_read_illegal!(out, $module, STATUS_INPUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_MFR_SPECIFIC);
            set_if_pmbus_read_illegal!(out, $module, STATUS_FANS_1_2);
            set_if_pmbus_read_illegal!(out, $module, STATUS_FANS_3_4);
            Capabilities(out)
        })
    };
}

use drv_i2c_types::pmbus_status::Capabilities;
type StatusRow = (&'static str, fn() -> Capabilities);

// Before you add a pmbus device to this list, you should make sure that you
// have reviewed the pmbus crate to make sure that any unsupported status
// registers are marked as illegal, similar to oxidecomputer/pmbus#35.
//
// Failure to do so could cause CML or OTHER error bits to be set. Just adding
// the device to this list (without accurate `pmbus` crate information) will
// likely make the compilation succeed, but should not be done for production
// devices where this may trigger runtime CML errors.
const PMBUS_GENERATOR: &[StatusRow] = &[
    generator!("adm127x", adm127x),
    generator!("bmr491", bmr491),
    generator!("isl68224", isl68224),
    generator!("lm5066", lm5066),
    generator!("lm5066i", lm5066i),
    generator!("mwocp67", mwocp67),
    generator!("mwocp68", mwocp68),
    generator!("raa229618", raa229618),
    generator!("raa229620a", raa229620a),
    generator!("tps546b24a", tps546b24a),
];
