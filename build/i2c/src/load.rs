// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 1: loading and deserializing the I2C section of an application
//! manifest.
//!
//! This module contains only the `serde` types describing the manifest, plus
//! thin helpers for loading them. It performs no validation beyond what
//! `serde` itself does.

use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Load the I2C configuration from the environment, as set up by `xtask dist`.
pub fn load_from_env() -> Result<Config> {
    build_util::config::<Config>()
        .map_err(|err| anyhow::anyhow!("malformed config.i2c: {err:?}"))
}

/// Parse an application manifest (or fragment thereof) from a TOML string.
pub fn parse_config(toml: &str) -> Result<Config> {
    build_util::toml_from_str(toml)
}

/// Parse just the I2C section of an application manifest (or fragment
/// thereof) from a TOML string.
pub fn parse(toml: &str) -> Result<I2cConfig> {
    Ok(parse_config(toml)?.i2c)
}
//
// Our definition of the `Config` type.  We share this type with all other
// build-specific types; we must not set `deny_unknown_fields` here.
//
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub i2c: I2cConfig,

    /// Sensors which are not attached via I2C (`[config.sensor]`), but which
    /// share the sensor ID space with the I2C sensors.
    pub sensor: Option<SensorConfig>,
}

/// The `[config.sensor]` section: sensors not attached via I2C.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SensorConfig {
    pub devices: Vec<OtherSensorDevice>,
}

/// A non-I2C device with sensors.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OtherSensorDevice {
    /// device name (must be unique among non-I2C devices)
    pub name: String,

    /// device part name
    pub device: String,

    /// description of device
    pub description: String,

    /// number of sensors of each kind
    pub sensors: BTreeMap<Sensor, usize>,

    /// reference designator, if any
    pub refdes: Option<Refdes>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct I2cConfig {
    pub controllers: Vec<I2cController>,
    pub devices: Option<Vec<I2cDevice>>,
}

//
// Note that [`ports`] is a `BTreeMap` (rather than, say, an `IndexMap`).
// This is load-bearing!  It is essential that deserialization of our
// application TOML have the same ordering for the ports, as the index is used
// by the debugger to denote a desired port.  One might think that an
// `IndexMap` would assure this, but because our configuration is reserialized
// as part of the build process (with the re-serialized TOML being stuffed
// into an environment variable), and because TOML is not stable with respect
// to the ordering of a table (both in terms of the specification -- see e.g.
// https://github.com/toml-lang/toml/issues/162 -- and in terms of the toml-rs
// implementation which, by default, uses a `BTreeMap` rather than an
// `IndexMap` for tables), we must be sure to impose our own (absolute)
// ordering.
//
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct I2cController {
    pub controller: u8,
    pub ports: BTreeMap<String, I2cPort>,
    #[serde(default)]
    pub target: bool,
}

//
// Unfortunately, the toml-rs parsing of enums isn't quite right (see
// https://github.com/alexcrichton/toml-rs/issues/390 for details).  As a
// result, we currently flatten what really should be enums around topology
// (i.e., [`controller`]/[`port`] vs. [`bus`]) and device class parameters
// (i.e., [`power`]) into optional fields in [`I2cDevice`].  This makes it
// easier to accidentally create invalid entries (e.g., a device that has both
// a controller *and* a named bus), so the validation code should go to
// additional lengths to assure that these mistakes are caught in compilation.
//

#[derive(Clone, Debug, Deserialize, PartialOrd, Ord, Eq, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(dead_code)]
pub struct I2cDevice {
    /// device part name
    pub device: String,

    /// device name
    pub name: Option<String>,

    /// I2C controller, if bus not named
    pub controller: Option<u8>,

    /// I2C bus name, if controller not specified
    pub bus: Option<String>,

    /// I2C port, if required
    pub port: Option<String>,

    /// Disambiguation between sensor configurations
    pub flavor: Option<String>,

    /// I2C address
    pub address: u8,

    /// I2C mux, if any
    pub mux: Option<u8>,

    /// I2C segment, if any
    pub segment: Option<u8>,

    /// description of device
    pub description: String,

    /// if this is an EEPROM, configures the format for VPD read from this
    /// EEPROM.
    ///
    /// providing a value for this is valid only if `device = "at24csw080"`.
    pub eeprom_vpd: Option<EepromVpd>,

    /// reference designator, if any
    pub refdes: Option<Refdes>,

    /// power information, if any
    pub power: Option<I2cPower>,

    /// sensor information, if any
    pub sensors: Option<I2cSensors>,

    /// device is removable
    #[serde(default)]
    pub removable: bool,

    /// We typically expect that each device will have a driver in
    /// `drv-i2c-devices` that implements the `Validate` trait, doing some
    /// device-specific validation (like checking the model number via PMBus).
    /// If there is no driver, then codegen will fail - _unless_ you set this to
    /// true to opt in to a generic, fallback implementation of `Validate` that
    /// just checks whether the device ACKs a single-byte i2c read (with no
    /// write beforehand). This is meant to detect whether the device is
    /// present. However, not all devices react to such a read in the same way,
    /// so this fallback implementation may be incorrect or cause unwanted side
    /// effects on some devices.
    #[serde(default)]
    pub validate_with_raw_read: bool,
}

impl I2cDevice {
    /// Checks whether the given sensor kind is associated with an `I2cPower`
    /// struct stored in this device, returning it if that's the case.
    ///
    /// In most cases, when the power member variable is present, sensors have a
    /// one-to-one association with power rails.  However, this isn't always
    /// true: in the power shelf, for example, there are two rails and three
    /// (uncorrelated) temperature sensors.
    ///
    /// This is indicated with the `sensors` array, which allows us to specify
    /// only certain kinds of sensors being tied to rails.
    ///
    /// If the `sensors` array is `None`, then we fall back to the default case
    /// of all sensors being one-to-one associated with rails.
    pub fn power_for_kind(&self, kind: Sensor) -> Option<&I2cPower> {
        self.power.as_ref().filter(|power| {
            power.sensors.as_ref().is_none_or(|s| s.contains(&kind))
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct I2cPort {
    pub name: Option<String>,
    #[allow(dead_code)]
    pub description: Option<String>,
    pub scl: I2cPin,
    pub sda: I2cPin,
    pub af: u8,
    #[serde(default)]
    pub muxes: Vec<I2cMux>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct I2cPin {
    pub gpio_port: Option<String>,
    pub pin: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct I2cGpio {
    pub port: String,
    pub pin: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct I2cMux {
    pub driver: String,
    pub address: u8,
    #[serde(alias = "enable")]
    pub nreset: Option<I2cGpio>,
}

#[derive(Clone, Debug, Deserialize, PartialOrd, PartialEq, Eq, Ord)]
#[allow(dead_code)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct I2cPower {
    pub rails: Option<Vec<String>>,

    /// Optional phases, which must be the same length as `rails` if present
    pub phases: Option<Vec<Vec<u8>>>,

    #[serde(default = "I2cPower::default_pmbus")]
    pub pmbus: bool,

    /// Lists which sensor types have a one-to-one association with power rails
    ///
    /// When `None`, we assume that all sensor types are mapped one-to-one with
    /// rails.  Otherwise, *only* the listed sensor types are associated with
    /// rails (which is the case in systems with independent temperature sensors
    /// and power rails).
    pub sensors: Option<Vec<Sensor>>,
}

impl I2cPower {
    fn default_pmbus() -> bool {
        true
    }
}

#[derive(Clone, Debug, Deserialize, PartialOrd, PartialEq, Eq, Ord)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(dead_code)]
pub struct I2cSensors {
    #[serde(default)]
    pub temperature: usize,

    #[serde(default)]
    pub power: usize,

    #[serde(default)]
    pub current: usize,

    #[serde(default)]
    pub voltage: usize,

    #[serde(default)]
    pub input_current: usize,

    #[serde(default)]
    pub input_voltage: usize,

    #[serde(default)]
    pub speed: usize,

    pub names: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Hash, PartialOrd, PartialEq, Eq, Ord)]
#[serde(untagged)]
pub enum Refdes {
    Component(String),
    Path(Vec<String>),
}

impl I2cSensors {
    /// Checks whether two sensor sets are compatible
    ///
    /// "Compatible" means that they have the same number of sensors in each
    /// category, meaning they can be represented by the same `struct`.
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        // Manually unpack the struct, so that any new sensor types have to be
        // updated here!
        let &Self {
            temperature,
            power,
            current,
            voltage,
            input_current,
            input_voltage,
            speed,
            names: _,
        } = self;
        temperature == other.temperature
            && power == other.power
            && current == other.current
            && voltage == other.voltage
            && input_current == other.input_current
            && input_voltage == other.input_voltage
            && speed == other.speed
    }
}

#[derive(
    Copy, Clone, Deserialize, Debug, PartialEq, Eq, Hash, Ord, PartialOrd,
)]
#[serde(rename_all = "kebab-case")]
pub enum Sensor {
    Temperature,
    Power,
    Current,
    Voltage,
    InputCurrent,
    InputVoltage,
    Speed,
    Pwm,
}

impl std::fmt::Display for Sensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Sensor::Temperature => "TEMPERATURE",
                Sensor::Power => "POWER",
                Sensor::Current => "CURRENT",
                Sensor::Voltage => "VOLTAGE",
                Sensor::InputCurrent => "INPUT_CURRENT",
                Sensor::InputVoltage => "INPUT_VOLTAGE",
                Sensor::Speed => "SPEED",
                Sensor::Pwm => "PWM",
            }
        )
    }
}

#[derive(
    Copy,
    Clone,
    Deserialize,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Ord,
    PartialOrd,
    Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum EepromVpd {
    #[default]
    SingleBarcode,
    SledFanTray,
}

impl Refdes {
    pub fn to_component_id(&self) -> String {
        self.join_with_case(str::make_ascii_uppercase, "/")
    }

    pub fn to_upper_ident(&self) -> String {
        self.join_with_case(str::make_ascii_uppercase, "_")
    }

    pub fn to_lower_ident(&self) -> String {
        self.join_with_case(str::make_ascii_lowercase, "_")
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Component(c) => c.len(),
            Self::Path(p) => {
                // length of each path component...
                p.iter().map(|s| s.len()).sum::<usize>()
                // ...plus separators
                + (p.len() - 1)
            }
        }
    }

    // This is never used but it's necessary to shut Clippy up
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn join_with_case(
        &self,
        change_case: impl Fn(&mut str),
        sep: &str,
    ) -> String {
        match self {
            Self::Component(s) => {
                let mut s = s.clone();
                change_case(&mut s);
                s
            }
            Self::Path(parts) => {
                let len = parts.iter().map(String::len).sum::<usize>()
                    + (parts.len() - 1) * sep.len();
                let mut s = String::with_capacity(len);
                let mut parts = parts.iter();
                if let Some(first) = parts.next() {
                    s.push_str(&first[..]);
                    for part in parts {
                        s.push_str(sep);
                        s.push_str(part);
                    }
                }
                change_case(&mut s);
                s
            }
        }
    }
}
