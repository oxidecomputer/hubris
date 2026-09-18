// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Code generation for the I2C configuration of an application.
//!
//! This crate is a three stage pipeline:
//!
//! 1. [`load`] deserializes the `[config.i2c]` section of an application
//!    manifest, doing no validation beyond what `serde` does for us.
//! 2. [`analysis`] resolves and validates that configuration, producing an
//!    [`analysis::Report`]: everything that code generation needs, in a plain
//!    data structure.
//! 3. [`codegen`] turns a [`analysis::Report`] into Rust source code.  It is a
//!    pure formatting stage: it validates nothing.

pub mod analysis;
pub mod codegen;
pub mod load;

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs::File;

pub use analysis::{
    AnalysisSettings, ControllerPort, ControllerRole, DeviceBus, DeviceGroup,
    DeviceKey, DeviceLookup, DeviceName, DeviceNameKey, DeviceRefdes,
    DeviceRefdesKey, DeviceSensor, I2cDeviceDescription, MuxSegment, NamedPort,
    OtherSensors, PmbusDeviceDescription, PmbusRailDescription, PowerRails,
    Report, SensorsDescription, VpdKind,
};
pub use codegen::Codegen;
pub use load::{
    Config, EepromVpd, I2cConfig, OtherSensorDevice, Refdes, Sensor,
    SensorConfig,
};

/// Outputs from code generation which may be used by other build scripts.
pub struct CodegenOutputs {
    /// The generated code that would be output to `i2c_config.rs`.
    pub code: String,
}

/// A preset bundle of code generation settings, describing what a task needs
/// out of the I2C configuration.
///
/// This exists for the convenience of `build.rs` callers; the underlying
/// settings ([`CodegenSettings`]) are more granular.
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

/// A single section of generated code.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Section {
    Controllers,
    Pins,
    Ports,
    Muxes,
    Devices,
    Sensors,
    Validation,

    /// The `other_sensors` module for non-I2C sensors (`[config.sensor]`).
    OtherSensors,

    /// A table mapping every sensor ID to its component ID.
    SensorIdToComponentId,

    /// A table mapping every sensor ID to its name.
    SensorIdToName,
}

#[derive(PartialEq, Copy, Clone, Debug)]
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
    /// The role the controllers of interest play: initiator v. target.
    pub role: ControllerRole,

    /// The sections of code to emit, in order.
    pub sections: Vec<Section>,

    /// If `true`, require that exactly one controller match our role.
    pub require_single_controller: bool,

    /// if `true`, include component ID string in output.
    ///
    /// this requires that the `"drv-i2c-api/component-id"` feature flag is
    /// enabled. if that feature flag is enabled, then this MUST also be
    /// enabled.
    pub component_ids: bool,

    /// The chip we are generating code for.
    pub codegen_target: CodegenTarget,

    /// List of drivers relevant to validation.
    ///
    /// This is `None` unless validation is being generated: computing the
    /// driver list has side effects on the build.
    pub drivers: Option<HashSet<String>>,
}

impl CodegenSettings {
    /// The subset of these settings which affect analysis.
    pub fn analysis_settings(&self) -> AnalysisSettings {
        AnalysisSettings {
            role: self.role,
            component_ids: self.component_ids,
            drivers: self.drivers.clone(),
            require_component_ids: false,
            max_component_id_len: None,
        }
    }

    /// Analyze a loaded configuration with these settings.
    pub fn analyze(&self, config: Config) -> Result<Report> {
        analysis::analyze(config, &self.analysis_settings())
    }
}

impl From<Disposition> for CodegenSettings {
    fn from(disposition: Disposition) -> Self {
        use Section::*;

        let (role, sections) = match disposition {
            Disposition::Target => {
                (ControllerRole::Target, vec![Controllers, Pins, Ports])
            }
            Disposition::Initiator => (
                ControllerRole::Initiator,
                vec![Controllers, Pins, Ports, Muxes],
            ),
            Disposition::Devices => {
                (ControllerRole::Initiator, vec![Devices, Ports])
            }
            Disposition::Sensors => {
                (ControllerRole::Initiator, vec![Devices, Sensors])
            }
            Disposition::Validation => {
                (ControllerRole::Initiator, vec![Devices, Validation])
            }
        };

        CodegenSettings {
            role,
            sections,
            //
            // If we have the role of a target, we expect exactly one
            // controller to be configured as a target; if none have been
            // specified, the task should be deconfigured.
            //
            require_single_controller: disposition == Disposition::Target,
            component_ids: cfg!(feature = "component-id"),
            codegen_target: CodegenTarget::from_env(),
            drivers: if disposition == Disposition::Validation {
                Some(calculate_validate_drivers().unwrap())
            } else {
                None
            },
        }
    }
}

/// Generate code for an analyzed configuration.
///
/// This does not write the output to a file, but returns the generated code in
/// the `code` field of the `CodegenOutputs` struct. Using [`codegen_to_file`]
/// will also write the generated code to the `i2c_config.rs` file for the task
/// currently being built.
pub fn codegen(
    report: Report,
    settings: &CodegenSettings,
) -> Result<CodegenOutputs> {
    if settings.require_single_controller {
        report.check_single_controller()?;
    }

    let g = Codegen {
        report: &report,
        codegen_target: settings.codegen_target,
    };

    let mut body = proc_macro2::TokenStream::new();
    for section in &settings.sections {
        body.extend(match section {
            Section::Controllers => g.generate_controllers()?,
            Section::Pins => g.generate_pins()?,
            Section::Ports => g.generate_ports()?,
            Section::Muxes => g.generate_muxes()?,
            Section::Devices => g.generate_devices()?,
            Section::Sensors => g.generate_sensors()?,
            Section::Validation => g.generate_validation()?,
            Section::OtherSensors => g.generate_other_sensors()?,
            Section::SensorIdToComponentId => {
                g.generate_sensor_id_to_component_id()?
            }
            Section::SensorIdToName => g.generate_sensor_id_to_name()?,
        });
    }
    let output = codegen::i2c_config_module(body).to_string();

    Ok(CodegenOutputs { code: output })
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

    let report = settings.analyze(load::load_from_env()?)?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("i2c_config.rs");
    let mut file = File::create(dest_path)?;

    let outputs = codegen(report, &settings)?;

    file.write_all(outputs.code.as_bytes())?;

    Ok(outputs)
}

///
/// Returns a list of I2C device descriptions.
///
/// The order of device descriptions matches the indexing used in the generated
/// `validate()` command.
///
pub fn device_descriptions() -> impl Iterator<Item = I2cDeviceDescription> {
    device_descriptions_with(&AnalysisSettings::for_device_descriptions())
        .unwrap()
        .into_iter()
}

/// Returns the I2C device descriptions for the task being built, analyzed
/// with the given settings.
///
/// Use [`AnalysisSettings::for_device_descriptions`] as a starting point and
/// tighten it (e.g. requiring component IDs) as needed. The order of the
/// descriptions matches the indexing used in the generated `validate()` and
/// `device_by_index()` functions.
pub fn device_descriptions_with(
    settings: &AnalysisSettings,
) -> Result<Vec<I2cDeviceDescription>> {
    let config = load::load_from_env()?;
    let report = analysis::analyze(config, settings)?;
    Ok(report.device_descriptions().collect())
}

impl AnalysisSettings {
    /// The settings used by [`device_descriptions`]: initiator controllers,
    /// component IDs per the `component-id` feature, and no validation
    /// drivers (which would have side effects on the build).
    pub fn for_device_descriptions() -> Self {
        Self {
            role: ControllerRole::Initiator,
            component_ids: cfg!(feature = "component-id"),
            drivers: None,
            require_component_ids: false,
            max_component_id_len: None,
        }
    }
}

/// Determine the set of I2C device drivers available for validation.
///
/// Note that this has side effects on the build: it emits a
/// `cargo::rerun-if-changed` directive for the driver source directory.
pub fn calculate_validate_drivers() -> Result<HashSet<String>> {
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
