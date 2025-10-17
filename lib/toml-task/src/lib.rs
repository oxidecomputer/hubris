// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `toml-task` allows for `xtask` and `build.rs` scripts to share a common
//! definition of a `task` within a TOML file.
use anyhow::{bail, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Represents a single `task` in a Hubris TOML file.
///
/// Parameterized by `T`, which is a type representing the configuration block.
/// In cases where this isn't strongly typed, `T` defaults to
/// [`ordered_toml::Value`], which can contain anything.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Task<T = ordered_toml::Value> {
    pub bin_crate: String,
    pub priority: u8,
    pub stacksize: Option<u32>,
    #[serde(default)]
    pub start: bool,

    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub notifications: Vec<String>,
    #[serde(default)]
    pub copy_to_archive: Vec<String>,

    /// Memory regions which should be mapped as accessible to this task
    #[serde(default)]
    pub extern_regions: Vec<String>,

    // Order matters here:
    // TOML serialization doesn't allow us to put a value type after any Table
    // type, so we put all of our `IndexMap` (and `config`, which often contains
    // a map under the hood)
    //
    // Note that `task_slots` serializes to a Sequence, not a Table, so it's not
    // grouped with the other `IndexMap`s below.
    #[serde(
        default,
        deserialize_with = "deserialize_task_slots",
        serialize_with = "serialize_task_slots"
    )]
    pub task_slots: IndexMap<String, String>,

    #[serde(default = "Option::default")]
    pub config: Option<T>,

    #[serde(default)]
    pub interrupts: IndexMap<String, String>,
    #[serde(default)]
    pub sections: IndexMap<String, String>,
    #[serde(default)]
    pub max_sizes: IndexMap<String, u32>,
    #[serde(default)]
    pub no_default_features: bool,
}

impl<T> Task<T> {
    pub fn notification_bit(&self, name: &str) -> Result<u8> {
        match self.notifications.iter().position(|n| n == name) {
            Some(i) => {
                if i < 32 {
                    Ok(i.try_into().unwrap())
                } else {
                    bail!("too many IRQs; {i} cannot fit in a `u32`")
                }
            }
            None => bail!(
                "could not find notification '{name}' \
                 (options are {:?})",
                self.notifications
            ),
        }
    }
    pub fn notification_mask(&self, name: &str) -> Result<u32> {
        Ok(1u32 << self.notification_bit(name)?)
    }
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
fn deserialize_task_slots<'de, D>(
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

/// Reverses `deserialize_task_slots`, turning each slot into a 1-item map
fn serialize_task_slots<S>(
    slots: &IndexMap<String, String>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;

    let mut seq = s.serialize_seq(Some(slots.len()))?;
    for (k, v) in slots.iter() {
        let mut i = IndexMap::new();
        i.insert(k, v);
        seq.serialize_element(&i)?;
    }
    seq.end()
}
