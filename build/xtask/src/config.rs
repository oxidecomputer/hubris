// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{hash_map::DefaultHasher, BTreeMap};
use std::hash::Hasher;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use indexmap::IndexMap;
use serde::Deserialize;

/// A `RawConfig` represents an `app.toml` file that has been deserialized,
/// but may not be ready for use.  In particular, we use the `chip` field
/// to load a second file containing peripheral register addresses.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawConfig {
    name: String,
    target: String,
    board: String,
    #[serde(default)]
    chip: Option<String>,
    #[serde(default)]
    signing: IndexMap<String, Signing>,
    secure_separation: Option<bool>,
    stacksize: Option<u32>,
    bootloader: Option<Bootloader>,
    kernel: Kernel,
    outputs: IndexMap<String, Output>,
    tasks: IndexMap<String, Task>,
    #[serde(default)]
    peripherals: IndexMap<String, Peripheral>,
    #[serde(default)]
    extratext: IndexMap<String, Peripheral>,
    supervisor: Option<Supervisor>,
    #[serde(default)]
    config: Option<ordered_toml::Value>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub target: String,
    pub board: String,
    pub chip: Option<String>,
    pub signing: IndexMap<String, Signing>,
    pub secure_separation: Option<bool>,
    pub stacksize: Option<u32>,
    pub bootloader: Option<Bootloader>,
    pub kernel: Kernel,
    pub outputs: IndexMap<String, Output>,
    pub tasks: IndexMap<String, Task>,
    pub peripherals: IndexMap<String, Peripheral>,
    pub extratext: IndexMap<String, Peripheral>,
    pub supervisor: Option<Supervisor>,
    pub config: Option<ordered_toml::Value>,
    pub buildhash: u64,
}

impl Config {
    pub fn from_file(cfg: &Path) -> Result<Self> {
        let cfg_contents = std::fs::read(&cfg)?;
        let toml: RawConfig = toml::from_slice(&cfg_contents)?;

        let mut hasher = DefaultHasher::new();
        hasher.write(&cfg_contents);

        // If the app.toml specifies a `chip` key, then load the peripheral
        // register map from a separate file and accumulate that file in the
        // buildhash.
        let peripherals = if let Some(chip) = &toml.chip {
            if !toml.peripherals.is_empty() {
                bail!("Cannot specify both chip and peripherals");
            }
            let chip_file = cfg.parent().unwrap().join(chip);
            let chip_contents = std::fs::read(chip_file)?;
            hasher.write(&chip_contents);
            toml::from_slice(&chip_contents)?
        } else {
            toml.peripherals
        };

        let buildhash = hasher.finish();

        Ok(Config {
            name: toml.name,
            target: toml.target,
            board: toml.board,
            chip: toml.chip,
            signing: toml.signing,
            secure_separation: toml.secure_separation,
            stacksize: toml.stacksize,
            bootloader: toml.bootloader,
            kernel: toml.kernel,
            outputs: toml.outputs,
            tasks: toml.tasks,
            peripherals,
            extratext: toml.extratext,
            supervisor: toml.supervisor,
            config: toml.config,
            buildhash,
        })
    }

    pub fn task_name_suggestion(&self, name: &str) -> String {
        // Suggest only for very small differences
        // High number can result in inaccurate suggestions for short queries e.g. `rls`
        const MAX_DISTANCE: usize = 3;

        let mut scored: Vec<_> = self
            .tasks
            .keys()
            .filter_map(|s| {
                let distance = strsim::damerau_levenshtein(name, s);
                if distance <= MAX_DISTANCE {
                    Some((distance, s))
                } else {
                    None
                }
            })
            .collect();
        scored.sort();
        let mut out = format!("'{}' is not a valid task name.", name);
        if let Some((_, s)) = scored.get(0) {
            out.push_str(&format!(" Did you mean '{}'?", s));
        }
        out
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Signing {
    pub method: String,
    pub priv_key: Option<PathBuf>,
    pub root_cert: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Bootloader {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub sections: IndexMap<String, String>,
    #[serde(default)]
    pub sharedsyms: Vec<String>,
    pub imagea_flash_start: u32,
    pub imagea_flash_size: u32,
    pub imagea_ram_start: u32,
    pub imagea_ram_size: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Kernel {
    pub path: PathBuf,
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Supervisor {
    pub notification: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Output {
    pub address: u32,
    pub size: u32,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub execute: bool,
    #[serde(default)]
    pub dma: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Task {
    pub path: PathBuf,
    pub name: String,
    pub requires: IndexMap<String, u32>,
    pub priority: u32,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub start: bool,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub interrupts: IndexMap<String, u32>,
    #[serde(default)]
    pub sections: IndexMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_task_slot")]
    pub task_slots: IndexMap<String, String>,
    #[serde(default)]
    pub config: Option<ordered_toml::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Peripheral {
    pub address: u32,
    pub size: u32,
    #[serde(default)]
    pub interrupts: BTreeMap<String, u32>,
}

/// In the common case, task slots map back to a task of the same name (e.g.
/// `gpio_driver`, `rcc_driver`).  However, certain tasks need generic task
/// slot names, e.g. they'll have a task slot named `spi_driver` which will
/// be mapped to a specific SPI driver task (`spi2_driver`).
///
/// This deserializer lets us handle both cases, while making the common case
/// easiest to write.  In `app.toml`, you can write something like
/// ```toml
/// task-slots = [
///     "gpio_driver",
///     "i2c_driver",
///     "rcc_driver",
///     {spi_driver: "spi2_driver"},
/// ]
/// ```
fn deserialize_task_slot<'de, D>(
    deserializer: D,
) -> Result<IndexMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Clone, Debug, Deserialize)]
    #[serde(untagged)]
    enum ArrayItem {
        Identity(String),
        Remap(IndexMap<String, String>),
    }
    let s: Vec<ArrayItem> = serde::Deserialize::deserialize(deserializer)?;
    let mut out = IndexMap::new();
    for a in s {
        match a {
            ArrayItem::Identity(s) => {
                out.insert(s.clone(), s.clone());
            }
            ArrayItem::Remap(m) => {
                if m.len() != 1 {
                    return Err(serde::de::Error::invalid_length(
                        m.len(),
                        &"a single key-value pair",
                    ));
                }
                let (k, v) = m.iter().next().unwrap();
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    Ok(out)
}
