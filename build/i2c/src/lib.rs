// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::load::{I2cController, I2cDevice};
use anyhow::{Context, Result};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::sync::Arc;

/// Outputs from code generation which may be used by other build scripts.
pub struct CodegenOutputs {
    /// The generated code that would be output to `i2c_config.rs`.
    pub code: String,
    /// If codegen was run with [`DispositionSensors`], the generated sensor
    /// description, which may be used as an input to other code generation
    /// steps.
    pub sensors: Option<I2cSensorsDescription>,
}

pub mod analysis;
pub mod codegen;
pub mod load;

pub use analysis::{
    DeviceKey, DeviceNameKey, DeviceRefdesKey, DeviceSensor,
    I2cSensorsDescription,
};
pub use load::{Config, EepromVpd, I2cConfig, Refdes, Sensor};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Disposition {
    /// controller is an initiator
    Initiator,

    /// controller is a target
    Target,

    /// devices are used (i.e., controller is not used), but not as sensors
    Devices,

    /// devices are used, with some used as sensors
    Sensors,

    /// devices are used, but only as validation
    Validation,
}

pub struct ConfigGenerator {
    /// Settings
    pub(crate) settings: CodegenSettings,

    /// all controllers
    pub(crate) controllers: Vec<I2cController>,

    /// all devices
    pub(crate) devices: Vec<I2cDevice>,

    /// hash bus name to controller/port index pair
    pub(crate) buses: HashMap<String, (u8, usize)>,

    /// hash controller/port pair to port index
    pub(crate) ports: IndexMap<(u8, String), usize>,

    /// hash of controllers to single port indices
    pub(crate) singletons: HashMap<u8, usize>,
}

fn calculate_validate_drivers() -> Result<HashSet<String>> {
    //
    // Lord, have mercy: we are going to find the crate containing i2c
    // devices, and go fishing for where we believe the device drivers
    // themselves to be.  It does not need to be said that this is
    // operating by convention; there are (many) ways to envision this
    // breaking -- with apologies, dear reader, if that's what brings you
    // here!
    //
    // Apology accepted. Perhaps this should be completely redesigned, but
    // I've at least made it so the build will now fail with a helpful error
    // message if the device driver isn't found.
    let mut drivers = std::collections::HashSet::new();

    use cargo_metadata::MetadataCommand;

    let metadata = MetadataCommand::new()
        .manifest_path("./Cargo.toml")
        .exec()
        .unwrap();

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name.as_str() == "drv-i2c-devices")
        .context("failed to find drv-i2c-devices")?;

    let dir = pkg
        .manifest_path
        .parent()
        .context("failed to get i2c device path")?;

    println!("cargo::rerun-if-changed={}", dir.join("src"));

    for entry in std::fs::read_dir(dir.join("src"))? {
        if let Some(f) = entry?.path().file_name()
            && let Some(name) = f.to_str().unwrap().strip_suffix(".rs")
        {
            drivers.insert(name.to_string());
        }
    }

    drivers.remove("lib");
    Ok(drivers)
}

pub const VPD_EEPROM_DEVICES: &[&str] = &["at24csw080"];
pub const VPD_TMP11X_DEVICES: &[&str] = &["tmp116", "tmp117"];

impl ConfigGenerator {
    pub fn new_with_config(settings: CodegenSettings, i2c: I2cConfig) -> Self {
        let mut controllers = vec![];
        let mut buses = HashMap::new();
        let mut ports = IndexMap::new();
        let mut singletons = HashMap::new();

        for c in i2c.controllers {
            //
            // We always insert our buses (even for controllers that don't
            // match our dispostion) to assure that devices can always find
            // their bus.
            //
            for (index, (p, port)) in c.ports.iter().enumerate() {
                if let Some(name) = &port.name
                    && buses
                        .insert(name.clone(), (c.controller, index))
                        .is_some()
                {
                    panic!("i2c bus {name} appears twice");
                }

                if c.ports.len() == 1 {
                    singletons.insert(c.controller, index);
                }

                ports.insert((c.controller, p.clone()), index);
            }

            if c.target != (settings.disposition == Disposition::Target) {
                continue;
            }

            controllers.push(c);
        }

        if let Some(devices) = &i2c.devices {
            for d in devices {
                match (d.controller, d.bus.as_ref()) {
                    (None, None) => {
                        panic!(
                            "device {} at address {:#x} must have \
                            a bus or controller",
                            d.device, d.address
                        );
                    }
                    (Some(_), Some(_)) => {
                        panic!(
                            "device {} at address {:#x} has both \
                            a bus and a controller",
                            d.device, d.address
                        );
                    }
                    (_, Some(bus)) if !buses.contains_key(bus) => {
                        panic!(
                            "device {} at address {:#x} specifies \
                            unknown bus \"{bus}\"",
                            d.device, d.address,
                        );
                    }
                    (_, _) => {}
                }
                if d.eeprom_vpd.is_some() {
                    assert!(
                        VPD_EEPROM_DEVICES.contains(&d.device.as_str()),
                        "device {} at address {:#x} is configured with an \
                         EEPROM VPD format, but it is not a supported EEPROM \
                         device (currently, we know about the following \
                         EEPROMs: {VPD_EEPROM_DEVICES:?})",
                        d.device,
                        d.address,
                    );
                }
            }
        }

        Self {
            devices: i2c.devices.unwrap_or_default(),
            controllers,
            buses,
            ports,
            singletons,
            settings,
        }
    }

    fn new(settings: CodegenSettings) -> Self {
        let i2c = match load::load_from_env() {
            Ok(i2c) => i2c,
            Err(err) => {
                panic!("{err:?}");
            }
        };

        Self::new_with_config(settings, i2c)
    }
}

#[derive(PartialEq, Copy, Clone)]
pub enum CodegenTarget {
    None,
    Stm32H743,
    Stm32H753,
    Stm32G031,
    Stm32G030,
}

impl CodegenTarget {
    fn from_env() -> Self {
        let h743 = build_util::has_feature("h743");
        let h753 = build_util::has_feature("h753");
        let g031 = build_util::has_feature("g031");
        let g030 = build_util::has_feature("g030");
        let mut count = 0;
        for sel in [h743, h753, g031, g030] {
            if sel {
                count += 1;
            }
        }

        if count > 1 {
            panic!("Too many features selected!");
        }

        if h743 {
            Self::Stm32H743
        } else if h753 {
            Self::Stm32H753
        } else if g031 {
            Self::Stm32G031
        } else if g030 {
            Self::Stm32G030
        } else {
            Self::None
        }
    }
}

#[derive(Clone)]
pub struct CodegenSettings {
    /// disposition of this configuration: target v. initiator v. devices
    pub disposition: Disposition,
    /// if `true`, include component ID string in output.
    ///
    /// this requires that the `"drv-i2c-api/component-id"` feature flag is
    /// enabled. if that feature flag is enabled, then this MUST also be
    /// enabled.
    pub component_ids: bool,
    pub codegen_target: CodegenTarget,
    /// List of drivers relevant to validation
    pub drivers: HashSet<String>,
}

impl From<Disposition> for CodegenSettings {
    fn from(disposition: Disposition) -> Self {
        CodegenSettings {
            disposition,
            component_ids: cfg!(feature = "component-id"),
            codegen_target: CodegenTarget::from_env(),
            drivers: if disposition == Disposition::Validation {
                calculate_validate_drivers().unwrap()
            } else {
                HashSet::new()
            },
        }
    }
}

/// Run code generation and write the output to an `i2c_config.rs` file in the
/// build output directory of the task being built.
///
/// This is the usual entrypoint for task `build.rs` files.
///
/// We (usually) take a `Disposition`, and automatically determine the output
/// as a predictable file name in the OUT directory.
pub fn codegen_to_file(
    settings: impl Into<CodegenSettings>,
) -> Result<CodegenOutputs> {
    use std::io::Write;

    let settings = settings.into();
    assert_eq!(cfg!(feature = "component-id"), settings.component_ids);

    let g = ConfigGenerator::new(settings);
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("i2c_config.rs");
    let mut file = File::create(dest_path)?;

    let outputs = g.codegen()?;

    file.write_all(outputs.code.as_bytes())?;

    Ok(outputs)
}

#[derive(Debug)]
pub struct I2cDeviceDescription {
    pub device: String,
    pub description: String,
    pub sensors: Vec<Arc<DeviceSensor>>,
    pub device_id: Option<String>,
    pub name: Option<String>,
    pub validate_with_raw_read: bool,
    pub eeprom_vpd: Option<EepromVpd>,
    /// If this is a PMBus device, this field contains additional data about the
    /// PMBus device to be used for generating PMBus-y code.
    pub pmbus: Option<PmbusDeviceDescription>,
}

#[derive(Debug, Clone)]
pub struct PmbusDeviceDescription {
    pub rails: Vec<PmbusRailDescription>,
}

#[derive(Debug, Clone)]
pub struct PmbusRailDescription {
    pub name: String,
    pub phases: Vec<u8>,
}

impl I2cDeviceDescription {
    /// Returns `true` if this device is a PMBus device.
    pub fn is_pmbus(&self) -> bool {
        self.pmbus.is_some()
    }
}

///
/// Returns a list of I2C device descriptions.
///
/// The order of device descriptions matches the indexing used in the generated
/// `validate()` command.
///
pub fn device_descriptions() -> impl Iterator<Item = I2cDeviceDescription> {
    let g = ConfigGenerator::new(Disposition::Validation.into());
    g.device_descriptions()
}

impl ConfigGenerator {
    /// Returns a list of I2C device descriptions for this configuration.
    ///
    /// See [`device_descriptions`] for details.
    pub fn device_descriptions(
        self,
    ) -> impl Iterator<Item = I2cDeviceDescription> {
        let g = self;
        let sensors = g.sensors_description();

        assert_eq!(sensors.device_sensors.len(), g.devices.len());

        // Matches the ordering of the `match` produced by `generate_validation()`
        // above; if we change the order here, it must change there as well.
        g.devices.into_iter().zip(sensors.device_sensors).map(
            |(device, sensors)| {
                let device_id =
                    device.refdes.as_ref().map(Refdes::to_component_id);
                let pmbus = device.power.as_ref().and_then(|power| {
                    if !power.pmbus {
                        return None;
                    }

                    let rails = match (
                        power.rails.as_ref(),
                        power.phases.as_ref(),
                    ) {
                        (Some(rails), Some(phases)) => {
                            assert_eq!(
                                rails.len(),
                                phases.len(),
                                "invalid config: PMBus device {device_id:?}'s \
                             `power.rails` and  `power.phases` lists are not \
                             the same length"
                            );
                            rails
                                .iter()
                                .cloned()
                                .zip(phases.iter().cloned())
                                .map(|(name, phases)| PmbusRailDescription {
                                    name,
                                    phases,
                                })
                                .collect()
                        }
                        (Some(rails), None) => rails
                            .iter()
                            .cloned()
                            .map(|name| PmbusRailDescription {
                                name,
                                phases: Vec::new(),
                            })
                            .collect(),
                        (None, Some(_)) => {
                            panic!(
                                "invalid config: PMBus device {device_id:?} \
                            defines a `power.phases` list, but not a \
                            `power.rails` list"
                            );
                        }
                        (None, None) => Vec::new(),
                    };

                    Some(PmbusDeviceDescription { rails })
                });

                I2cDeviceDescription {
                    device: device.device,
                    description: device.description,
                    sensors,
                    device_id,
                    name: device.name,
                    validate_with_raw_read: device.validate_with_raw_read,
                    eeprom_vpd: device.eeprom_vpd,
                    pmbus,
                }
            },
        )
    }
}
