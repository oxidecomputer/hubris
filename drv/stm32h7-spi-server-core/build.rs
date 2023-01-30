// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, bail, Result};
use build_spi::*;
use indexmap::IndexMap;
use quote::ToTokens;
use std::collections::BTreeMap;
use std::io::Write;

fn main() -> Result<()> {
    build_util::expose_target_board();

    let full_task_config = build_util::task_full_config_toml()?;
    let spi = check_uses_and_interrupts(
        &full_task_config.uses,
        &full_task_config.interrupts,
    )?;

    // Confirm that we've enabled the appropriate SPI feature, and *not* enabled
    // any other SPI features.
    let feat = format!("CARGO_FEATURE_{}", spi.to_uppercase());
    if std::env::var(&feat).is_err() {
        bail!("when using {spi} peripheral, '{spi}' feature must be enabled");
    }
    if let Some(f) = std::env::vars()
        .map(|(k, _v)| k)
        .filter(|k| k.starts_with("CARGO_FEATURE_SPI"))
        .find(|f| f != &feat)
    {
        bail!(
            "cannot have feature '{}' defined when using peripheral {spi}",
            f.trim_start_matches("CARGO_FEATURE_").to_lowercase()
        );
    }

    let global_config = build_util::config::<SpiGlobalConfig>()?;
    check_spi_config(&global_config.spi, &spi)?;
    generate_spi_config(&global_config.spi, &spi)?;

    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// SPI config code generation.
//
// Our config types, by design, map almost directly onto the structs that the
// SPI driver uses to configure itself. This means we can do the code generation
// process in a separable-and-composable fashion, by implementing
// `quote::ToTokens` for most of the config types.
//
// Each impl defines, in isolation, how code generation works for that part of
// the config. This is most of the code generation implementation; the
// `generate_spi_config` routine is just a wrapper.

fn generate_spi_config(
    config: &BTreeMap<String, SpiConfig>,
    global_config: &str,
) -> Result<()> {
    let config = config.get(global_config).ok_or_else(|| {
        anyhow!("reference to undefined spi config {}", global_config)
    })?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("spi_config.rs");

    let mut out = std::fs::File::create(&dest_path)?;

    writeln!(out, "{}", config.to_token_stream())?;

    drop(out);

    call_rustfmt::rustfmt(&dest_path)?;

    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// Check routines.

fn check_uses_and_interrupts(
    uses: &[String],
    interrupts: &IndexMap<String, String>,
) -> Result<String> {
    let mut spi = None;

    // 10 SPI peripherals should be enough for anyone
    let re = regex::Regex::new(r"^spi\d$").unwrap();
    for p in uses {
        if re.is_match(p) {
            if let Some(q) = spi {
                bail!("multiple SPI periperals in use: {p} and {q}");
            }
            spi = Some(p);
        }
    }
    let spi = match spi {
        Some(s) => s,
        None => bail!("No SPI peripheral in {uses:?}"),
    };
    let spi_irq = format!("{spi}.irq");
    if !interrupts.contains_key(&spi_irq) {
        bail!("interrupts should contain '{spi_irq}'");
    }
    Ok(spi.to_owned())
}

fn check_spi_config(
    config: &BTreeMap<String, SpiConfig>,
    global_config: &str,
) -> Result<()> {
    // We only want to look at the subset of global configuration relevant to
    // this task, so that error reporting is more focused.
    let config = config.get(global_config).ok_or_else(|| {
        anyhow!("reference to undefined spi config {}", global_config)
    })?;

    if config.controller < 1 || config.controller > 6 {
        return Err(anyhow!(
            "bad controller {}, valid values are 1 thru 6",
            config.controller
        )
        .into());
    }

    for mux in config.mux_options.values() {
        for out in &mux.outputs {
            check_afpinset(out)?;
        }
        check_afpin(&mux.input)?;
    }

    for (devname, dev) in &config.devices {
        if !config.mux_options.contains_key(&dev.mux) {
            return Err(anyhow!(
                "device {} names undefined mux {}",
                devname,
                dev.mux
            )
            .into());
        }

        for pin in &dev.cs {
            check_gpiopin(pin)?;
        }
    }

    Ok(())
}

fn check_afpinset(config: &AfPinSetConfig) -> Result<()> {
    for &pin in &config.pins {
        if pin > 15 {
            return Err(anyhow!(
                "pin {:?}{} is invalid, pins are numbered 0-15",
                config.port,
                pin
            )
            .into());
        }
    }
    if config.af.0 > 15 {
        return Err(anyhow!(
            "af {:?} is invalid, functions are numbered 0-15",
            config.af
        )
        .into());
    }
    Ok(())
}

fn check_afpin(config: &AfPinConfig) -> Result<()> {
    check_gpiopin(&config.pc)?;
    if config.af.0 > 15 {
        return Err(anyhow!(
            "af {:?} is invalid, functions are numbered 0-15",
            config.af
        )
        .into());
    }
    Ok(())
}

fn check_gpiopin(config: &GpioPinConfig) -> Result<()> {
    if config.pin > 15 {
        return Err(anyhow!(
            "pin {:?}{} is invalid, pins are numbered 0-15",
            config.port,
            config.pin
        )
        .into());
    }
    Ok(())
}
