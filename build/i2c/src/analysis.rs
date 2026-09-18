// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 2: analysis of a loaded configuration.
//!
//! This stage takes the raw, deserialized [`I2cConfig`] produced by the
//! [`crate::load`] stage and resolves it into a [`Report`]: a plain data
//! structure containing everything that code generation needs, already
//! resolved and validated.
//!
//! All of the validation of an application manifest lives here; code
//! generation is a pure function of the [`Report`].

use crate::load::{
    Config, EepromVpd, I2cController, I2cDevice, I2cSensors, OtherSensorDevice,
    Refdes, Sensor, SensorConfig,
};
use anyhow::{Result, bail};
use convert_case::{Case, Casing};
use iddqd::IdOrdMap;
use multimap::MultiMap;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Devices whose VPD is read from an EEPROM.
const VPD_EEPROM_DEVICES: &[&str] = &["at24csw080"];
/// Devices whose VPD is read from TMP11x-style EEPROM registers.
const VPD_TMP11X_DEVICES: &[&str] = &["tmp116", "tmp117"];

/// How a device's vital product data (VPD) is read, as far as the manifest
/// can tell.
///
/// Consumers may know of additional VPD sources (e.g. PMBus devices whose
/// drivers support the manufacturer registers); this only describes what
/// follows from the device type in the manifest.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VpdKind {
    /// An EEPROM, in the given format.
    Eeprom(EepromVpd),
    /// A TMP116/TMP117 temperature sensor's EEPROM registers.
    Tmp11x,
}

impl VpdKind {
    /// Classifies a device by its type (and EEPROM format, if any).
    pub(crate) fn of(d: &I2cDevice) -> Option<Self> {
        let device = d.device.as_str();
        if VPD_EEPROM_DEVICES.contains(&device) {
            Some(Self::Eeprom(d.eeprom_vpd.unwrap_or_default()))
        } else if VPD_TMP11X_DEVICES.contains(&device) {
            Some(Self::Tmp11x)
        } else {
            None
        }
    }
}

/// The role that the I2C controllers of interest play.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ControllerRole {
    /// We care about controllers acting as initiators.
    #[default]
    Initiator,
    /// We care about controllers acting as targets.
    Target,
}

/// Knobs affecting analysis.
#[derive(Clone, Debug, Default)]
pub struct AnalysisSettings {
    /// Which controllers are selected.
    pub role: ControllerRole,

    /// If `true`, resolve component IDs for each device.
    pub component_ids: bool,

    /// The set of drivers available for validation.
    ///
    /// This is `None` unless validation is actually being generated, because
    /// computing the set of drivers has side effects on the build (it runs
    /// `cargo metadata` and emits `cargo::rerun-if-changed` directives).
    pub drivers: Option<HashSet<String>>,

    /// If `true`, every device must have a component ID (i.e. a refdes).
    ///
    /// Component IDs are always checked for uniqueness across all devices;
    /// this additionally makes their absence an error, for consumers that
    /// need to address every device by ID.
    pub require_component_ids: bool,

    /// If set, no component ID may be longer than this many bytes.
    pub max_component_id_len: Option<usize>,
}

/// A port on a controller, identified by the controller number and the
/// port's index among that controller's ports (in manifest order).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControllerPort {
    /// The controller number (e.g. `2` for `I2C2`).
    pub controller: u8,

    /// The index of the port on that controller.
    pub index: usize,
}

/// A port as named in the manifest (e.g. `B`), resolved to its location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedPort {
    /// The port's name in the manifest.
    pub name: String,

    /// Where the port is.
    pub port: ControllerPort,
}

/// The mux (1-indexed) and segment behind which a device lives.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MuxSegment {
    pub mux: u8,
    pub segment: u8,
}

/// A group of devices sharing a key, as indices into [`Report::devices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceGroup<K> {
    /// What the devices have in common.
    pub key: K,

    /// The devices, in manifest order.
    pub indices: Vec<usize>,
}

/// A single device found by a key which is unique to it, as an index into
/// [`Report::devices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceLookup<K> {
    /// The key which identifies the device.
    pub key: K,

    /// The device.
    pub index: usize,
}

/// A device type (e.g. `tmp117`) on a named bus.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceBus {
    pub device: String,
    pub bus: String,
}

/// A device type (e.g. `tmp117`) with a name from the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceName {
    pub device: String,
    pub name: String,
}

/// A device type (e.g. `tmp117`) with a reference designator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceRefdes {
    pub device: String,
    pub refdes: Refdes,
}

/// The power rails of a configuration, split by how they are exposed.
#[derive(Debug, Default)]
pub struct PowerRails {
    /// Every rail, in rail-name order (exposed in the generated `pmbus`
    /// module).
    pub pmbus: Vec<PowerRail>,

    /// Only the rails on non-PMBus devices, in rail-name order (exposed in
    /// the generated `power` module).
    pub non_pmbus: Vec<PowerRail>,
}

/// A device, with everything about its position on the bus resolved.
#[derive(Clone, Debug)]
pub struct Device {
    /// The device as it appeared in the manifest.
    pub config: I2cDevice,

    /// The controller and port this device hangs off of.
    pub location: ControllerPort,

    /// The mux and segment this device lives behind, if any.
    pub segment: Option<MuxSegment>,

    /// The component ID of this device, if component IDs were requested and
    /// this device has a reference designator.
    pub component_id: Option<String>,

    /// PMBus details for this device, if it is a PMBus device.
    pub pmbus: Option<PmbusDeviceDescription>,
}

/// A single power rail, resolved to the device that provides it.
#[derive(Clone, Debug)]
pub struct PowerRail {
    /// The name of the rail.
    pub rail: String,

    /// Index into [`Report::devices`] of the device providing this rail.
    pub device: usize,

    /// The bank (i.e. rail index) within the device, if the device has more
    /// than one rail.
    pub bank: Option<usize>,

    /// The phases of this rail, if the device declares phases.
    pub phases: Option<Vec<u8>>,
}

/// How a device's sensors are described by a generated `struct`.
#[derive(Clone, Debug)]
pub struct SensorStruct {
    /// The name of the `Sensors_`-prefixed type for this device.
    pub name: String,

    /// If `true`, this device is the first user of `name`, and the type must
    /// be declared before use.
    pub declare: bool,

    /// Labels under which the sensor constants for this device are emitted.
    pub labels: Vec<String>,
}

/// How a device is validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Validation {
    /// The device has a driver in `drv-i2c-devices` with this (UpperCamel)
    /// type name, which implements `Validate`.
    Driver(String),

    /// The device has opted in to the generic "does it ACK a read?" check.
    RawRead,
}

/// Everything that code generation needs, resolved and validated.
#[derive(Debug)]
pub struct Report {
    /// The role that [`Report::controllers`] were selected for.
    pub role: ControllerRole,

    /// If `true`, component IDs were requested.
    pub component_ids: bool,

    /// The controllers matching our role.
    pub controllers: Vec<I2cController>,

    /// Map of (controller, port name) to port index, in manifest order.
    pub ports: Vec<NamedPort>,

    /// Map of bus name to (controller, port index).
    pub buses: HashMap<String, ControllerPort>,

    /// All devices, in manifest order.  This order is load-bearing: it is the
    /// order used by `device_by_index()` and `validate()` in the generated
    /// code, as well as by [`Report::device_descriptions`].
    pub devices: Vec<Device>,

    /// Devices grouped by device name, sorted by device name.
    pub by_device: Vec<DeviceGroup<String>>,

    /// Devices grouped by (device name, bus name), sorted.
    pub by_bus: Vec<DeviceGroup<DeviceBus>>,

    /// Devices by (device name, name), sorted.
    pub by_name: Vec<DeviceLookup<DeviceName>>,

    /// Devices by (device name, refdes), sorted.
    pub by_refdes: Vec<DeviceLookup<DeviceRefdes>>,

    /// Devices grouped by controller, sorted by controller.
    pub by_controller: Vec<DeviceGroup<u8>>,

    /// Devices grouped by port index, sorted by port index.
    pub by_port: Vec<DeviceGroup<usize>>,

    /// The longest component ID of any device with a refdes.
    pub max_component_id_len: usize,

    /// The power rails, split by how they are exposed.
    pub rails: PowerRails,

    /// The sensors of every device.
    pub sensors: SensorsDescription,

    /// Per-device sensor `struct` information, parallel to [`Report::devices`].
    pub sensor_structs: Vec<SensorStruct>,

    /// Per-device validation strategy, parallel to [`Report::devices`].  This
    /// is `None` unless validation was requested.
    pub validation: Option<Vec<Validation>>,
}

impl Report {
    /// Checks that exactly one controller was selected.
    ///
    /// If we have the role of a target, we expect exactly one controller to be
    /// configured as a target; if none have been specified, the task should be
    /// deconfigured.
    pub fn check_single_controller(&self) -> Result<()> {
        let n = self.controllers.len();

        if n != 1 {
            bail!("found {n} I2C controller(s); expected exactly one");
        }

        Ok(())
    }

    ///
    /// Returns a list of I2C device descriptions.
    ///
    /// The order of device descriptions matches the indexing used in the
    /// generated `validate()` command.
    ///
    pub fn device_descriptions(
        &self,
    ) -> impl Iterator<Item = I2cDeviceDescription> {
        self.devices
            .iter()
            .zip(self.sensors.device_sensors.iter())
            .map(|(device, sensors)| I2cDeviceDescription {
                device: device.config.device.clone(),
                description: device.config.description.clone(),
                sensors: sensors.clone(),
                device_id: device
                    .config
                    .refdes
                    .as_ref()
                    .map(Refdes::to_component_id),
                name: device.config.name.clone(),
                validate_with_raw_read: device.config.validate_with_raw_read,
                vpd: VpdKind::of(&device.config),
                pmbus: device.pmbus.clone(),
            })
    }
}

/// Analyze a loaded configuration, producing a [`Report`].
pub fn analyze(config: Config, settings: &AnalysisSettings) -> Result<Report> {
    let Config {
        i2c: config,
        sensor: other_sensors,
    } = config;

    let mut controllers = vec![];
    let mut buses = HashMap::new();
    let mut ports = Vec::new();
    let mut singletons = HashMap::new();

    for c in &config.controllers {
        //
        // We always insert our buses (even for controllers that don't
        // match our role) to assure that devices can always find their bus.
        //
        for (index, (p, port)) in c.ports.iter().enumerate() {
            let location = ControllerPort {
                controller: c.controller,
                index,
            };

            if let Some(name) = &port.name
                && buses.insert(name.clone(), location).is_some()
            {
                bail!("i2c bus {name} appears twice");
            }

            if c.ports.len() == 1 {
                singletons.insert(c.controller, index);
            }

            ports.push(NamedPort {
                name: p.clone(),
                port: location,
            });
        }

        if c.target != (settings.role == ControllerRole::Target) {
            continue;
        }

        controllers.push(c.clone());
    }

    let config_devices = config.devices.unwrap_or_default();

    check_devices(&config_devices, &buses)?;

    //
    // Resolve each device onto a controller, port, and (optionally) a mux
    // segment.
    //
    let mut devices = Vec::with_capacity(config_devices.len());

    for d in &config_devices {
        let location = lookup_controller_port(d, &buses, &ports, &singletons)?;

        let segment = lookup_segment(d, &config.controllers, location)?;

        let component_id = if settings.component_ids {
            if let Some(ref refdes) = d.refdes {
                Some(refdes.to_component_id())
            } else {
                println!(
                    "cargo::error=device {} has no refdes, but we were asked to generate component IDs",
                    d.device
                );
                None
            }
        } else {
            None
        };

        devices.push(Device {
            config: d.clone(),
            location,
            segment,
            component_id,
            pmbus: None,
        });
    }

    let groups = group_devices(&devices)?;

    check_component_ids(&config_devices, settings)?;

    let rails = analyze_power(&devices)?;

    // Now that power has been checked, we can describe the PMBus devices.
    for d in &mut devices {
        d.pmbus = pmbus_description(&d.config);
    }

    let sensors =
        SensorsDescription::new(&config_devices, other_sensors.as_ref())?;
    let sensor_structs = analyze_sensor_structs(&config_devices)?;

    let validation = match &settings.drivers {
        Some(drivers) => Some(analyze_validation(&config_devices, drivers)?),
        None => None,
    };

    Ok(Report {
        role: settings.role,
        component_ids: settings.component_ids,
        controllers,
        ports,
        buses,
        devices,
        by_device: groups.by_device,
        by_bus: groups.by_bus,
        by_name: groups.by_name,
        by_refdes: groups.by_refdes,
        by_controller: groups.by_controller,
        by_port: groups.by_port,
        max_component_id_len: groups.max_component_id_len,
        rails,
        sensors,
        sensor_structs,
        validation,
    })
}

/// Checks the parts of a device's configuration that don't depend on anything
/// else having been resolved.
fn check_devices(
    devices: &[I2cDevice],
    buses: &HashMap<String, ControllerPort>,
) -> Result<()> {
    for d in devices {
        match (d.controller, d.bus.as_ref()) {
            (None, None) => {
                bail!(
                    "device {} at address {:#x} must have a bus or controller",
                    d.device,
                    d.address
                );
            }
            (Some(_), Some(_)) => {
                bail!(
                    "device {} at address {:#x} has both a bus and a \
                     controller",
                    d.device,
                    d.address
                );
            }
            (_, Some(bus)) if !buses.contains_key(bus) => {
                bail!(
                    "device {} at address {:#x} specifies unknown bus \"{bus}\"",
                    d.device,
                    d.address,
                );
            }
            (_, _) => {}
        }

        if d.eeprom_vpd.is_some()
            && !VPD_EEPROM_DEVICES.contains(&d.device.as_str())
        {
            bail!(
                "device {} at address {:#x} is configured with an EEPROM VPD \
                 format, but it is not a supported EEPROM device (currently, \
                 we know about the following EEPROMs: {VPD_EEPROM_DEVICES:?})",
                d.device,
                d.address,
            );
        }
    }

    Ok(())
}

/// Checks the component IDs (derived from refdes) across all devices.
///
/// IDs must be unique across the whole manifest, regardless of device type.
/// Depending on `settings`, every device may also be required to have one,
/// and IDs may be limited in length. All problems are reported together so
/// that a manifest can be fixed in one pass.
fn check_component_ids(
    devices: &[I2cDevice],
    settings: &AnalysisSettings,
) -> Result<()> {
    let mut problems = vec![];
    let mut seen: HashMap<String, &I2cDevice> = HashMap::new();

    for d in devices {
        let Some(refdes) = &d.refdes else {
            if settings.require_component_ids {
                problems.push(format!(
                    "device {:?} ({:?}) has no component ID (refdes)",
                    d.device, d.description
                ));
            }
            continue;
        };

        let id = refdes.to_component_id();

        if let Some(max) = settings.max_component_id_len
            && id.len() > max
        {
            problems.push(format!(
                "component ID {id:?} for device {:?} exceeds the maximum \
                 length ({max} bytes)",
                d.device
            ));
        }

        if let Some(prev) = seen.insert(id.clone(), d) {
            problems.push(format!(
                "duplicate component ID {id:?}: used by both {:?} ({:?}) and \
                 {:?} ({:?})",
                prev.device, prev.description, d.device, d.description
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        bail!(
            "{} component ID problem(s):\n  {}",
            problems.len(),
            problems.join("\n  ")
        );
    }
}

fn lookup_controller_port(
    d: &I2cDevice,
    buses: &HashMap<String, ControllerPort>,
    ports: &[NamedPort],
    singletons: &HashMap<u8, usize>,
) -> Result<ControllerPort> {
    let controller = match &d.bus {
        Some(bus) => buses[bus].controller,
        None => d.controller.unwrap(),
    };

    let index = match (&d.bus, &d.port) {
        (Some(_), Some(_)) => {
            bail!("device {} has both port and bus", d.device);
        }

        (Some(bus), None) => match buses.get(bus) {
            Some(port) => port.index,
            None => {
                bail!("device {} has invalid bus", d.device);
            }
        },

        (None, Some(name)) => {
            match ports
                .iter()
                .find(|p| p.port.controller == controller && p.name == *name)
            {
                None => {
                    bail!("device {} has invalid port", d.device);
                }
                Some(port) => port.port.index,
            }
        }

        //
        // We allow ports to be unspecified if the specified
        // controller has only a single port; check the singletons.
        //
        (None, None) => match singletons.get(&controller) {
            Some(&index) => index,
            None => {
                bail!("device {} has ambiguous port", d.device)
            }
        },
    };

    Ok(ControllerPort { controller, index })
}

fn lookup_segment(
    d: &I2cDevice,
    all_controllers: &[I2cController],
    location: ControllerPort,
) -> Result<Option<MuxSegment>> {
    match (d.mux, d.segment) {
        (Some(mux), Some(segment)) => {
            let mux_count = all_controllers
                .iter()
                .find(|c| c.controller == location.controller)
                .and_then(|c| c.ports.values().nth(location.index))
                .map(|p| p.muxes.len())
                .unwrap_or(0);

            if mux == 0 {
                bail!(
                    "invalid mux value of 0 for {d:?} \
                    (note that muxes are 1-indexed)"
                );
            } else if usize::from(mux) > mux_count {
                bail!("invalid mux {mux} for {d:?} (must be <= {mux_count})");
            }

            Ok(Some(MuxSegment { mux, segment }))
        }
        (None, None) => Ok(None),
        (Some(_), None) => {
            bail!("device {} specifies a mux but no segment", d.device)
        }
        (None, Some(_)) => {
            bail!("device {} specifies a segment but no mux", d.device)
        }
    }
}

#[derive(Default)]
struct Groups {
    by_device: Vec<DeviceGroup<String>>,
    by_bus: Vec<DeviceGroup<DeviceBus>>,
    by_name: Vec<DeviceLookup<DeviceName>>,
    by_refdes: Vec<DeviceLookup<DeviceRefdes>>,
    by_controller: Vec<DeviceGroup<u8>>,
    by_port: Vec<DeviceGroup<usize>>,
    max_component_id_len: usize,
}

/// Collects a multimap into sorted [`DeviceGroup`]s.
fn groups<K: Ord + Eq + std::hash::Hash>(
    map: MultiMap<K, usize>,
) -> Vec<DeviceGroup<K>> {
    let mut groups: Vec<_> = map
        .into_iter()
        .map(|(key, indices)| DeviceGroup { key, indices })
        .collect();
    groups.sort_by(|a, b| a.key.cmp(&b.key));
    groups
}

/// Collects a map into sorted [`DeviceLookup`]s.
fn lookups<K: Ord>(map: HashMap<K, usize>) -> Vec<DeviceLookup<K>> {
    let mut lookups: Vec<_> = map
        .into_iter()
        .map(|(key, index)| DeviceLookup { key, index })
        .collect();
    lookups.sort_by(|a, b| a.key.cmp(&b.key));
    lookups
}

fn group_devices(devices: &[Device]) -> Result<Groups> {
    //
    // Throw all devices into a MultiMap based on device.
    //
    let mut by_device = MultiMap::new();
    let mut by_name = HashMap::new();
    let mut by_refdes = HashMap::new();
    let mut by_bus = MultiMap::new();
    let mut by_port = MultiMap::new();
    let mut by_controller = MultiMap::new();

    for (index, dev) in devices.iter().enumerate() {
        let d = &dev.config;

        by_device.insert(d.device.clone(), index);
        by_port.insert(dev.location.index, index);
        by_controller.insert(dev.location.controller, index);

        if let Some(bus) = &d.bus {
            let key = DeviceBus {
                device: d.device.clone(),
                bus: bus.clone(),
            };
            by_bus.insert(key, index);
        }

        if let Some(name) = &d.name {
            let key = DeviceName {
                device: d.device.clone(),
                name: name.clone(),
            };
            if by_name.insert(key, index).is_some() {
                bail!("duplicate name {} for device {}", name, d.device)
            }
        }

        if let Some(refdes) = &d.refdes {
            let key = DeviceRefdes {
                device: d.device.clone(),
                refdes: refdes.clone(),
            };
            let as_name = DeviceName {
                device: d.device.clone(),
                name: refdes.to_upper_ident(),
            };
            if by_refdes.insert(key, index).is_some() {
                bail!("duplicate refdes {refdes:?} for device {}", d.device)
            } else if by_name.contains_key(&as_name) {
                bail!(
                    "refdes {refdes:?} for device {} is also a device name",
                    d.device
                )
            }
        }
    }

    let mut groups = Groups {
        by_device: groups(by_device),
        by_bus: groups(by_bus),
        by_name: lookups(by_name),
        by_refdes: lookups(by_refdes),
        by_controller: groups(by_controller),
        by_port: groups(by_port),
        max_component_id_len: 0,
    };

    for lookup in &groups.by_refdes {
        groups.max_component_id_len =
            groups.max_component_id_len.max(lookup.key.refdes.len());
    }

    Ok(groups)
}

fn analyze_power(devices: &[Device]) -> Result<PowerRails> {
    // Keyed by rail name, which also gives us the rail-name ordering.
    let mut byrail: BTreeMap<String, PowerRail> = BTreeMap::new();

    for (index, dev) in devices.iter().enumerate() {
        let d = &dev.config;

        if let Some(power) = &d.power {
            //
            // If we have phases, we must have phases for each rail -- and
            // we check that no single phase is present in more than one
            // rail.
            //
            match (&power.rails, &power.phases) {
                (Some(_), None) | (None, None) => {}
                (Some(r), Some(p)) if r.len() == p.len() => {
                    let mut all = HashSet::new();

                    if let Some(p) =
                        p.iter().flatten().find(|&p| !all.insert(p))
                    {
                        bail!("phase {p} appears multiple times in {d:?}");
                    }
                }
                _ => {
                    bail!("rail/phase length mismatch on {d:?}");
                }
            }

            if let Some(rails) = &power.rails {
                let single = rails.len() == 1;
                for (rindex, rail) in rails.iter().enumerate() {
                    if rail.is_empty() {
                        continue;
                    }

                    let bank = if single { None } else { Some(rindex) };
                    let phases = power
                        .phases
                        .as_ref()
                        .map(|phases| phases[bank.unwrap_or(0)].clone());

                    let entry = PowerRail {
                        rail: rail.clone(),
                        device: index,
                        bank,
                        phases,
                    };

                    if byrail.insert(rail.clone(), entry).is_some() {
                        bail!("duplicate rail {rail}");
                    }
                }
            }
        }
    }

    let mut rails = PowerRails::default();

    for entry in byrail.into_values() {
        let power = devices[entry.device].config.power.as_ref();

        //
        // Note that the `pmbus` module contains *every* power rail, while the
        // `power` module contains only those rails on non-PMBus devices.
        //
        if !power.is_some_and(|p| p.pmbus) {
            rails.non_pmbus.push(entry.clone());
        }

        rails.pmbus.push(entry);
    }

    Ok(rails)
}

fn pmbus_description(d: &I2cDevice) -> Option<PmbusDeviceDescription> {
    let power = d.power.as_ref()?;

    if !power.pmbus {
        return None;
    }

    // Note that `analyze_power` has already verified that `rails` and
    // `phases` are consistent with one another.
    let rails = match (power.rails.as_ref(), power.phases.as_ref()) {
        (Some(rails), Some(phases)) => rails
            .iter()
            .cloned()
            .zip(phases.iter().cloned())
            .map(|(name, phases)| PmbusRailDescription { name, phases })
            .collect(),
        (Some(rails), None) => rails
            .iter()
            .cloned()
            .map(|name| PmbusRailDescription {
                name,
                phases: Vec::new(),
            })
            .collect(),
        (None, _) => Vec::new(),
    };

    Some(PmbusDeviceDescription { rails })
}

fn analyze_sensor_structs(devices: &[I2cDevice]) -> Result<Vec<SensorStruct>> {
    let mut emitted_structs: HashMap<String, Option<I2cSensors>> =
        HashMap::new();
    let mut out = Vec::with_capacity(devices.len());

    for d in devices {
        let mut struct_name = d.device.clone();
        if let Some(suffix) = &d.flavor {
            struct_name = format!("{struct_name}_{suffix}");
        }

        let declare = if let Some(prev) = emitted_structs.get(&struct_name) {
            match (prev, &d.sensors) {
                (Some(a), Some(b)) => {
                    if !a.is_compatible_with(b) {
                        bail!(
                            "I2C device {struct_name} is declared with \
                             inconsistent numbers of sensors.  Add a \
                             `flavor = \"...\"` key to disambiguate."
                        );
                    }
                }
                (Some(..), None) | (None, Some(..)) => {
                    bail!(
                        "I2C device {struct_name} is declared both \
                         with and without sensors.  Use a \
                         `flavor = \"...\"` key to disambiguate."
                    );
                }
                (None, None) => (),
            }
            false
        } else {
            emitted_structs.insert(struct_name.clone(), d.sensors.clone());
            true
        };

        let mut labels = vec![];
        if let Some(name) = &d.name {
            labels.push(name.to_uppercase());
        }
        if let Some(refdes) = &d.refdes {
            labels.push(refdes.to_upper_ident());
        }

        out.push(SensorStruct {
            name: struct_name,
            declare,
            labels,
        });
    }

    Ok(out)
}

fn analyze_validation(
    devices: &[I2cDevice],
    drivers: &HashSet<String>,
) -> Result<Vec<Validation>> {
    let mut out = Vec::with_capacity(devices.len());

    for device in devices {
        let describe = || {
            format!(
                "{}{}{}",
                device.device,
                device
                    .name
                    .as_ref()
                    .map(|name| format!(" {}", name))
                    .unwrap_or_default(),
                device
                    .refdes
                    .as_ref()
                    .map(|refdes| format!(" {:?}", refdes))
                    .unwrap_or_default()
            )
        };

        if drivers.contains(&device.device) {
            if device.validate_with_raw_read {
                bail!(
                    "Device '{}' set `validate-with-raw-read = true`, \
                    but that was probably a mistake because this device \
                    already has a driver in `drv-i2c-devices` that should \
                    be able to perform better, device-specific validation.",
                    describe(),
                );
            }

            out.push(Validation::Driver(
                device.device.to_case(Case::UpperCamel),
            ));
        } else {
            if !device.validate_with_raw_read {
                bail!(
                    "Device '{}' has no driver in `drv-i2c-devices`. \
                    You must either add a driver that implements the \
                    `Validate` trait or set `validate-with-raw-read = \
                    true` to opt in to a generic implementation instead.",
                    describe(),
                );
            }

            out.push(Validation::RawRead);
        }
    }

    Ok(out)
}

#[derive(Debug)]
pub struct I2cDeviceDescription {
    pub device: String,
    pub description: String,
    pub sensors: Vec<Arc<DeviceSensor>>,
    pub device_id: Option<String>,
    pub name: Option<String>,
    pub validate_with_raw_read: bool,
    /// How this device's VPD is read, if the device type has VPD.
    pub vpd: Option<VpdKind>,
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
#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceKey {
    pub device: String,
    pub kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceNameKey {
    pub device: String,
    pub name: String,
    pub kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceRefdesKey {
    pub device: String,
    pub refdes: Refdes,
    pub kind: Sensor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSensor {
    pub refdes: Option<Refdes>,
    pub name: Option<String>,
    pub kind: Sensor,
    pub id: usize,
}

impl iddqd::IdOrdItem for DeviceSensor {
    type Key<'a> = &'a usize;

    /// Retrieves the key.
    fn key(&self) -> Self::Key<'_> {
        &self.id
    }

    iddqd::id_upcast!();
}

#[derive(Debug)]
pub struct SensorsDescription {
    // In all maps below, the value is the sensor ID. The same sensor ID
    // can show up in multiple (including all!) of these maps.
    //
    // All sensors are guaranteed to be present in `by_device`, but
    // may not be present in the other maps (devices may or may not have a
    // name/bus in app.toml).
    /// `by_device` tracks items by what the sensor is (e.g. "Temperature"),
    /// and by what kind of device the sensor exists within, e.g. "TMP117".
    /// A `(Temperature, TMP117)` may have multiple sensor IDs that match the
    /// same tuple of options.
    pub(crate) by_device: BTreeMap<DeviceKey, Vec<usize>>,
    /// `by_refdes` tracks items on the two items above, PLUS what "refdes"
    /// is available, for example "U32". A `(Speed, Max31790, U32)` may
    /// have multiple sensor IDs, for example if there are 6 separate speed
    /// sensors hosted on the same physical I2C device.
    pub(crate) by_refdes: BTreeMap<DeviceRefdesKey, Vec<usize>>,
    /// `by_name` tracks items on the triple of device/name/kind, for example
    /// `(TMP117, "North", Temperature)`. This actually should probably not
    /// ever match multiple items, and will be fixed in the future. See
    /// https://github.com/oxidecomputer/hubris/issues/2637 for details.
    pub(crate) by_name: BTreeMap<DeviceNameKey, Vec<usize>>,
    /// All sensors by their ID
    pub by_id: IdOrdMap<Arc<DeviceSensor>>,

    /// list of all devices and a list of their sensors, with an optional sensor
    /// name (if present)
    pub(crate) device_sensors: Vec<Vec<Arc<DeviceSensor>>>,

    /// The number of I2C sensors; their IDs are `0..total_i2c_sensors`.
    pub total_i2c_sensors: usize,

    /// Non-I2C sensors (from `[config.sensor]`), in manifest order. Their
    /// IDs follow the I2C sensors' IDs.
    pub other_sensors: Vec<OtherSensors>,

    /// The number of non-I2C sensors.
    pub total_other_sensors: usize,
}

/// The sensors of a non-I2C device.
#[derive(Debug)]
pub struct OtherSensors {
    /// The device's manifest entry.
    pub config: OtherSensorDevice,

    /// The IDs of this device's sensors, by kind, in allocation order.
    pub ids_by_kind: BTreeMap<Sensor, Vec<usize>>,
}

impl std::fmt::Display for SensorsDescription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let SensorsDescription {
            by_device,
            by_name,
            by_refdes,
            by_id,
            device_sensors,
            total_i2c_sensors,
            other_sensors,
            total_other_sensors,
        } = self;
        writeln!(f, "by_device:")?;
        for (k, vs) in by_device {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_name:")?;
        for (k, vs) in by_name {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_refdes:")?;
        for (k, vs) in by_refdes {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_id:")?;
        for s in by_id {
            writeln!(f, "  - {s:?}")?;
        }
        writeln!(f, "device_sensors:")?;
        for (i, d) in device_sensors.iter().enumerate() {
            writeln!(f, "  - I2cDevice({i})")?;
            for (j, s) in d.iter().enumerate() {
                writeln!(f, "    - {i}.{j}: {s:?}")?;
            }
        }
        writeln!(f, "total_i2c_sensors: {total_i2c_sensors}")?;
        writeln!(f, "other_sensors:")?;
        for (i, d) in other_sensors.iter().enumerate() {
            writeln!(
                f,
                "  - OtherSensorDevice({i}) {:?} ({:?}, {:?}, refdes {:?})",
                d.config.name,
                d.config.device,
                d.config.description,
                d.config.refdes
            )?;
            for (kind, ids) in &d.ids_by_kind {
                writeln!(f, "    - {kind:?} :: {ids:?}")?;
            }
        }
        writeln!(f, "total_other_sensors: {total_other_sensors}")
    }
}

impl SensorsDescription {
    pub(crate) fn new(
        devices: &[I2cDevice],
        other: Option<&SensorConfig>,
    ) -> Result<Self> {
        let mut desc = Self {
            by_device: BTreeMap::new(),
            by_name: BTreeMap::new(),
            by_refdes: BTreeMap::new(),
            by_id: IdOrdMap::new(),
            device_sensors: vec![Vec::new(); devices.len()],
            total_i2c_sensors: 0,
            other_sensors: Vec::new(),
            total_other_sensors: 0,
        };

        for (d_index, d) in devices.iter().enumerate() {
            if let Some(s) = &d.sensors {
                for i in 0..s.temperature {
                    desc.add_sensor(Sensor::Temperature, d, i, d_index)?;
                }

                for i in 0..s.power {
                    desc.add_sensor(Sensor::Power, d, i, d_index)?;
                }

                for i in 0..s.current {
                    desc.add_sensor(Sensor::Current, d, i, d_index)?;
                }

                for i in 0..s.voltage {
                    desc.add_sensor(Sensor::Voltage, d, i, d_index)?;
                }

                for i in 0..s.input_current {
                    desc.add_sensor(Sensor::InputCurrent, d, i, d_index)?;
                }

                for i in 0..s.input_voltage {
                    desc.add_sensor(Sensor::InputVoltage, d, i, d_index)?;
                }

                for i in 0..s.speed {
                    desc.add_sensor(Sensor::Speed, d, i, d_index)?;
                }
            }
        }

        if let Some(other) = other {
            desc.add_other_sensors(other)?;
        }

        Ok(desc)
    }

    /// Allocates IDs for the non-I2C sensors, after all of the I2C sensors.
    fn add_other_sensors(&mut self, other: &SensorConfig) -> Result<()> {
        let mut names = std::collections::BTreeSet::new();

        for d in &other.devices {
            if !names.insert(d.name.clone()) {
                bail!("Duplicate sensor name: {}", d.name);
            }

            let mut ids_by_kind = BTreeMap::new();

            for (&kind, &count) in &d.sensors {
                let ids: &mut Vec<usize> = ids_by_kind.entry(kind).or_default();
                for _ in 0..count {
                    let id = self.total_i2c_sensors + self.total_other_sensors;
                    self.total_other_sensors += 1;

                    let sensor = Arc::new(DeviceSensor {
                        refdes: d.refdes.clone(),
                        name: Some(d.name.clone()),
                        kind,
                        id,
                    });

                    if let Err(prev) = self.by_id.insert_unique(sensor.clone())
                    {
                        bail!(
                            "weird: colliding sensor ID {id}: {prev:?} and \
                             {sensor:?}"
                        );
                    }

                    ids.push(id);
                }
            }

            self.other_sensors.push(OtherSensors {
                config: d.clone(),
                ids_by_kind,
            });
        }

        Ok(())
    }

    // `idx` is the index of the type of sensor within `d` (the idx-th
    // temperature sensor or the idx-th power sensor, etc.; see the loop in
    // `new()` above).
    //
    // `dev_index` is the index of `d` within the total list of devices.
    //
    // This method should only be called by `new()`. It fills out `self`'s
    // fields as it is being constructed.
    fn add_sensor(
        &mut self,
        kind: Sensor,
        d: &I2cDevice,
        idx: usize,
        dev_index: usize,
    ) -> Result<()> {
        let id = self.total_i2c_sensors;
        self.total_i2c_sensors += 1;

        let name: Option<String> = if let Some(power) = d.power_for_kind(kind) {
            if let Some(rails) = &power.rails {
                if idx < rails.len() {
                    Some(rails[idx].clone())
                } else {
                    bail!("sensor count exceeds rails for {d:?}");
                }
            } else {
                d.name.clone()
            }
        } else if let Some(names) = &d.sensors.as_ref().unwrap().names {
            if idx >= names.len() {
                bail!(
                    "name array is too short ({}) for sensor index ({idx})",
                    names.len(),
                );
            } else {
                Some(names[idx].clone())
            }
        } else {
            d.name.clone()
        };

        if let Some(name) = name.clone() {
            self.by_name
                .entry(DeviceNameKey {
                    device: d.device.clone(),
                    name,
                    kind,
                })
                .or_default()
                .push(id);
        }

        let sensor = Arc::new(DeviceSensor {
            refdes: d.refdes.clone(),
            name: name.clone(),
            kind,
            id,
        });

        if let Some(ref refdes) = sensor.refdes {
            self.by_refdes
                .entry(DeviceRefdesKey {
                    device: d.device.clone(),
                    refdes: refdes.clone(),
                    kind,
                })
                .or_default()
                .push(id);
        }

        self.by_device
            .entry(DeviceKey {
                device: d.device.clone(),
                kind,
            })
            .or_default()
            .push(id);

        if let Err(prev) = self.by_id.insert_unique(sensor.clone()) {
            bail!("weird: colliding sensor ID {id}: {prev:?} and {sensor:?}");
        };

        self.device_sensors[dev_index].push(sensor);

        Ok(())
    }
}
