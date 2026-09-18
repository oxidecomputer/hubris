// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for stage 2 of the pipeline: analyzing a loaded manifest.

use anyhow::Result;
use build_i2c::analysis::{self, AnalysisSettings, ControllerRole, Report};
use build_i2c::load;
use std::collections::HashSet;

//
// Two controllers: controller 2 has two ports (and so requires devices to name
// one), while controller 3 has a single port (with a mux on it).
//
const CONTROLLERS: &str = r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
name = "bus1"
scl = { pin = 10 }
sda = { pin = 11 }
af = 4

[i2c.controllers.ports.F]
name = "bus2"
scl = { pin = 12 }
sda = { pin = 13 }
af = 4

[[i2c.controllers]]
controller = 3

[i2c.controllers.ports.A]
name = "solo"
scl = { pin = 1 }
sda = { pin = 2 }
af = 4
muxes = [{ driver = "pca9548", address = 0x70 }]
"#;

fn settings(role: ControllerRole) -> AnalysisSettings {
    AnalysisSettings {
        role,
        ..Default::default()
    }
}

/// Analyze a manifest fragment appended to the standard controllers.
fn analyze(devices: &str) -> Result<Report> {
    analyze_with(
        &format!("{CONTROLLERS}{devices}"),
        settings(ControllerRole::Initiator),
    )
}

fn analyze_with(toml: &str, settings: AnalysisSettings) -> Result<Report> {
    analysis::analyze(load::parse_config(toml)?, &settings)
}

/// Asserts that analysis fails with an error containing `needle`.
#[track_caller]
fn assert_error(devices: &str, needle: &str) {
    match analyze(devices) {
        Ok(_) => panic!("expected an error containing {needle:?}"),
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                msg.contains(needle),
                "error {msg:?} does not contain {needle:?}"
            );
        }
    }
}

fn device(extra: &str) -> String {
    format!(
        r#"
[[i2c.devices]]
device = "tmp117"
address = 0x48
description = "a temperature sensor"
{extra}
"#
    )
}

//
// Resolution
//

#[test]
fn resolves_bus_to_controller_and_port() {
    let report = analyze(&device(r#"bus = "bus2""#)).unwrap();
    assert_eq!(report.devices[0].controller, 2);
    assert_eq!(report.devices[0].port, 1);

    let report = analyze(&device(r#"bus = "bus1""#)).unwrap();
    assert_eq!(report.devices[0].controller, 2);
    assert_eq!(report.devices[0].port, 0);
}

#[test]
fn resolves_explicit_port() {
    let report = analyze(&device("controller = 2\nport = \"F\"")).unwrap();
    assert_eq!(report.devices[0].controller, 2);
    assert_eq!(report.devices[0].port, 1);
}

#[test]
fn resolves_singleton_port() {
    // Controller 3 has exactly one port, so naming a port is optional.
    let report = analyze(&device("controller = 3")).unwrap();
    assert_eq!(report.devices[0].controller, 3);
    assert_eq!(report.devices[0].port, 0);
}

#[test]
fn resolves_mux_and_segment() {
    let report =
        analyze(&device("controller = 3\nmux = 1\nsegment = 4")).unwrap();
    assert_eq!(report.devices[0].segment, Some((1, 4)));

    let report = analyze(&device("controller = 3")).unwrap();
    assert_eq!(report.devices[0].segment, None);
}

#[test]
fn registers_buses_from_every_controller() {
    // Buses are registered from all controllers, even those that don't match
    // our role, so that devices can always find their bus.
    let report = analyze_with(
        &format!("{CONTROLLERS}{}", device(r#"bus = "bus1""#)),
        settings(ControllerRole::Target),
    )
    .unwrap();

    assert!(report.controllers.is_empty());
    assert_eq!(report.devices[0].controller, 2);
    assert_eq!(report.ports.len(), 3);
    assert_eq!(report.buses.len(), 3);
}

//
// Topology errors
//

#[test]
fn error_no_bus_or_controller() {
    assert_error(&device(""), "must have a bus or controller");
}

#[test]
fn error_both_bus_and_controller() {
    assert_error(
        &device("controller = 2\nbus = \"bus1\""),
        "has both a bus and a controller",
    );
}

#[test]
fn error_unknown_bus() {
    assert_error(
        &device(r#"bus = "nonesuch""#),
        "specifies unknown bus \"nonesuch\"",
    );
}

#[test]
fn error_both_port_and_bus() {
    assert_error(
        &device("bus = \"bus1\"\nport = \"B\""),
        "has both port and bus",
    );
}

#[test]
fn error_invalid_port() {
    assert_error(&device("controller = 2\nport = \"Q\""), "has invalid port");
}

#[test]
fn error_ambiguous_port() {
    // Controller 2 has two ports, so one must be named.
    assert_error(&device("controller = 2"), "has ambiguous port");
}

#[test]
fn error_mux_without_segment() {
    assert_error(
        &device("controller = 3\nmux = 1"),
        "specifies a mux but no segment",
    );
}

#[test]
fn error_segment_without_mux() {
    assert_error(
        &device("controller = 3\nsegment = 1"),
        "specifies a segment but no mux",
    );
}

#[test]
fn error_mux_zero() {
    assert_error(
        &device("controller = 3\nmux = 0\nsegment = 1"),
        "invalid mux value of 0",
    );
}

#[test]
fn error_mux_out_of_range() {
    assert_error(
        &device("controller = 3\nmux = 2\nsegment = 1"),
        "invalid mux 2",
    );
}

#[test]
fn error_bus_appears_twice() {
    let toml = format!(
        r#"{CONTROLLERS}
[[i2c.controllers]]
controller = 4

[i2c.controllers.ports.C]
name = "bus1"
scl = {{ pin = 1 }}
sda = {{ pin = 2 }}
af = 4
"#
    );

    let err =
        analyze_with(&toml, settings(ControllerRole::Initiator)).unwrap_err();
    assert!(
        format!("{err:#}").contains("i2c bus bus1 appears twice"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn error_eeprom_vpd_on_non_eeprom() {
    assert_error(
        &device("bus = \"bus1\"\neeprom-vpd = \"single-barcode\""),
        "is not a supported EEPROM device",
    );
}

//
// Naming errors
//

#[test]
fn error_duplicate_name() {
    let devices = format!(
        "{}{}",
        device("bus = \"bus1\"\nname = \"north\""),
        device("bus = \"bus2\"\nname = \"north\"")
    );
    assert_error(&devices, "duplicate name north for device tmp117");
}

#[test]
fn error_duplicate_refdes() {
    let devices = format!(
        "{}{}",
        device("bus = \"bus1\"\nrefdes = \"U1\""),
        device("bus = \"bus2\"\nrefdes = \"U1\"")
    );
    assert_error(&devices, "duplicate refdes");
}

#[test]
fn error_refdes_collides_with_name() {
    let devices = format!(
        "{}{}",
        device("bus = \"bus1\"\nname = \"U1\""),
        device("bus = \"bus2\"\nrefdes = \"U1\"")
    );
    assert_error(&devices, "is also a device name");
}

//
// Power errors
//

fn power_device(power: &str) -> String {
    format!(
        r#"
[[i2c.devices]]
device = "raa229618"
bus = "bus1"
address = 0x35
description = "a power controller"
power = {{ {power} }}
"#
    )
}

#[test]
fn resolves_power_rails() {
    let report = analyze(&power_device(r#"rails = ["V1", "V2"]"#)).unwrap();

    assert_eq!(report.pmbus_rails.len(), 2);
    assert_eq!(report.pmbus_rails[0].rail, "V1");
    assert_eq!(report.pmbus_rails[0].bank, Some(0));
    assert_eq!(report.pmbus_rails[1].rail, "V2");
    assert_eq!(report.pmbus_rails[1].bank, Some(1));

    // These rails are on a PMBus device, so they are not in `power`.
    assert!(report.power_rails.is_empty());

    // A single rail has no bank...
    let report = analyze(&power_device(r#"rails = ["V1"]"#)).unwrap();
    assert_eq!(report.pmbus_rails[0].bank, None);

    // ...and a non-PMBus device's rails appear in both lists.
    let report =
        analyze(&power_device(r#"rails = ["V1"], pmbus = false"#)).unwrap();
    assert_eq!(report.pmbus_rails.len(), 1);
    assert_eq!(report.power_rails.len(), 1);
}

#[test]
fn error_rail_phase_length_mismatch() {
    assert_error(
        &power_device(r#"rails = ["V1", "V2"], phases = [[0, 1]]"#),
        "rail/phase length mismatch",
    );
}

#[test]
fn error_duplicate_phase() {
    assert_error(
        &power_device(r#"rails = ["V1", "V2"], phases = [[0, 1], [1]]"#),
        "phase 1 appears multiple times",
    );
}

#[test]
fn error_duplicate_rail() {
    let devices = format!(
        "{}{}",
        power_device(r#"rails = ["V1"]"#),
        power_device(r#"rails = ["V1"]"#)
    );
    assert_error(&devices, "duplicate rail V1");
}

//
// Sensor errors
//

#[test]
fn error_sensor_count_exceeds_rails() {
    let devices = r#"
[[i2c.devices]]
device = "raa229618"
bus = "bus1"
address = 0x35
description = "a power controller"
power = { rails = ["V1"] }
sensors = { voltage = 2 }
"#;
    assert_error(devices, "sensor count exceeds rails");
}

#[test]
fn error_name_array_too_short() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a temperature sensor"
sensors = { temperature = 2, names = ["north"] }
"#;
    assert_error(devices, "name array is too short (1) for sensor index (1)");
}

#[test]
fn error_inconsistent_sensors() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "one"
sensors = { temperature = 1 }

[[i2c.devices]]
device = "tmp117"
bus = "bus2"
address = 0x49
description = "two"
sensors = { temperature = 2 }
"#;
    assert_error(devices, "inconsistent numbers of sensors");
}

#[test]
fn flavor_disambiguates_inconsistent_sensors() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "one"
sensors = { temperature = 1 }

[[i2c.devices]]
device = "tmp117"
bus = "bus2"
address = 0x49
description = "two"
flavor = "double"
sensors = { temperature = 2 }
"#;
    let report = analyze(devices).unwrap();
    assert_eq!(report.sensor_structs[0].name, "tmp117");
    assert!(report.sensor_structs[0].declare);
    assert_eq!(report.sensor_structs[1].name, "tmp117_double");
    assert!(report.sensor_structs[1].declare);
}

#[test]
fn error_with_and_without_sensors() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "one"
sensors = { temperature = 1 }

[[i2c.devices]]
device = "tmp117"
bus = "bus2"
address = 0x49
description = "two"
"#;
    assert_error(devices, "declared both with and without sensors");
}

#[test]
fn sensor_ids_are_assigned_in_order() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "one"
name = "north"
sensors = { temperature = 2, voltage = 1 }

[[i2c.devices]]
device = "tmp117"
bus = "bus2"
address = 0x49
description = "two"
name = "south"
sensors = { temperature = 2, voltage = 1 }
"#;
    let report = analyze(devices).unwrap();
    assert_eq!(report.sensors.total_i2c_sensors, 6);

    //
    // Sensors are numbered device by device, and within a device in the order
    // temperature, power, current, voltage, ...
    //
    let ids: Vec<_> = report
        .sensors
        .by_id
        .iter()
        .map(|s| (s.id, s.kind.to_string(), s.name.clone()))
        .collect();

    assert_eq!(
        ids,
        vec![
            (0, "TEMPERATURE".to_string(), Some("north".to_string())),
            (1, "TEMPERATURE".to_string(), Some("north".to_string())),
            (2, "VOLTAGE".to_string(), Some("north".to_string())),
            (3, "TEMPERATURE".to_string(), Some("south".to_string())),
            (4, "TEMPERATURE".to_string(), Some("south".to_string())),
            (5, "VOLTAGE".to_string(), Some("south".to_string())),
        ]
    );
}

#[test]
fn power_sensors_subset_names_only_those_kinds() {
    //
    // `power.sensors` limits which sensor kinds are named after rails; other
    // kinds fall back to the `names` array.
    //
    let devices = r#"
[[i2c.devices]]
device = "mwocp68"
bus = "bus1"
address = 0x40
description = "a power shelf"
name = "psu"
power = { rails = ["V54_PSU"], sensors = ["voltage"] }
sensors = { temperature = 2, voltage = 1, names = ["inlet", "outlet"] }
"#;
    let report = analyze(devices).unwrap();

    let names: Vec<_> = report
        .sensors
        .by_id
        .iter()
        .map(|s| (s.kind.to_string(), s.name.clone().unwrap()))
        .collect();

    assert_eq!(
        names,
        vec![
            ("TEMPERATURE".to_string(), "inlet".to_string()),
            ("TEMPERATURE".to_string(), "outlet".to_string()),
            ("VOLTAGE".to_string(), "V54_PSU".to_string()),
        ]
    );
}

//
// Roles
//

const TARGET_CONTROLLER: &str = r#"
[[i2c.controllers]]
controller = 7
target = true

[i2c.controllers.ports.B]
scl = { pin = 1 }
sda = { pin = 2 }
af = 4
"#;

#[test]
fn role_selects_controllers() {
    let toml = format!("{CONTROLLERS}{TARGET_CONTROLLER}");

    let initiator =
        analyze_with(&toml, settings(ControllerRole::Initiator)).unwrap();
    assert_eq!(initiator.controllers.len(), 2);
    initiator.check_single_controller().unwrap_err();

    let target = analyze_with(&toml, settings(ControllerRole::Target)).unwrap();
    assert_eq!(target.controllers.len(), 1);
    assert_eq!(target.controllers[0].controller, 7);
    target.check_single_controller().unwrap();
}

#[test]
fn error_no_target_controller() {
    let report =
        analyze_with(CONTROLLERS, settings(ControllerRole::Target)).unwrap();

    let err = report.check_single_controller().unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "found 0 I2C controller(s); expected exactly one"
    );
}

#[test]
fn error_two_target_controllers() {
    let toml = format!(
        "{CONTROLLERS}{TARGET_CONTROLLER}{}",
        TARGET_CONTROLLER.replace("controller = 7", "controller = 8")
    );

    let report = analyze_with(&toml, settings(ControllerRole::Target)).unwrap();

    let err = report.check_single_controller().unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "found 2 I2C controller(s); expected exactly one"
    );
}

//
// Validation
//

fn validation_settings(drivers: &[&str]) -> AnalysisSettings {
    AnalysisSettings {
        role: ControllerRole::Initiator,
        component_ids: false,
        drivers: Some(
            drivers
                .iter()
                .map(|d| d.to_string())
                .collect::<HashSet<_>>(),
        ),
        ..Default::default()
    }
}

#[test]
fn validation_strategies() {
    let devices = format!(
        "{}{}",
        device("bus = \"bus1\""),
        r#"
[[i2c.devices]]
device = "nonesuch"
bus = "bus2"
address = 0x20
description = "a device with no driver"
validate-with-raw-read = true
"#
    );

    let report = analyze_with(
        &format!("{CONTROLLERS}{devices}"),
        validation_settings(&["tmp117"]),
    )
    .unwrap();

    let validation = report.validation.unwrap();
    assert_eq!(
        validation,
        vec![
            analysis::Validation::Driver("Tmp117".to_string()),
            analysis::Validation::RawRead,
        ]
    );
}

#[test]
fn error_driver_with_raw_read() {
    let err = analyze_with(
        &format!(
            "{CONTROLLERS}{}",
            device("bus = \"bus1\"\nvalidate-with-raw-read = true")
        ),
        validation_settings(&["tmp117"]),
    )
    .unwrap_err();

    assert!(
        format!("{err:#}")
            .contains("set `validate-with-raw-read = true`, but that was"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn error_no_driver_without_raw_read() {
    let err = analyze_with(
        &format!("{CONTROLLERS}{}", device("bus = \"bus1\"")),
        validation_settings(&[]),
    )
    .unwrap_err();

    assert!(
        format!("{err:#}").contains("has no driver in `drv-i2c-devices`"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn validation_is_opt_in() {
    // Without drivers, no validation analysis is performed -- and so a device
    // with no driver is not an error.
    let report = analyze(&device("bus = \"bus1\"")).unwrap();
    assert!(report.validation.is_none());
}

//
// Groupings
//

#[test]
fn groups_devices() {
    let devices = r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
name = "north"
refdes = "U1"
address = 0x48
description = "one"

[[i2c.devices]]
device = "tmp117"
bus = "bus2"
name = "south"
address = 0x49
description = "two"

[[i2c.devices]]
device = "at24csw080"
controller = 3
address = 0x50
description = "three"
eeprom-vpd = "single-barcode"
"#;
    let report = analyze(devices).unwrap();

    assert_eq!(
        report.by_device,
        vec![
            ("at24csw080".to_string(), vec![2]),
            ("tmp117".to_string(), vec![0, 1]),
        ]
    );
    assert_eq!(
        report.by_bus,
        vec![
            (("tmp117".to_string(), "bus1".to_string()), vec![0]),
            (("tmp117".to_string(), "bus2".to_string()), vec![1]),
        ]
    );
    assert_eq!(report.by_controller, vec![(2, vec![0, 1]), (3, vec![2])]);
    assert_eq!(report.by_port, vec![(0, vec![0, 2]), (1, vec![1])]);
    assert_eq!(report.max_component_id_len, 2);
}

#[test]
fn component_ids_are_resolved_on_request() {
    let devices = device("bus = \"bus1\"\nrefdes = [\"J1\", \"U7\"]");

    let report = analyze(&devices).unwrap();
    assert_eq!(report.devices[0].component_id, None);

    let report = analyze_with(
        &format!("{CONTROLLERS}{devices}"),
        AnalysisSettings {
            component_ids: true,
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap();
    assert_eq!(report.devices[0].component_id.as_deref(), Some("J1/U7"));
}

#[test]
fn error_duplicate_component_id_across_device_types() {
    // The same refdes on two different device types is still one component.
    let err = analyze(
        r#"
[[i2c.devices]]
device = "tmp117"
refdes = "U7"
bus = "bus1"
address = 0x48
description = "a temperature sensor"

[[i2c.devices]]
device = "at24csw080"
refdes = "U7"
bus = "bus2"
address = 0x50
description = "an eeprom"
"#,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("duplicate component ID \"U7\""), "{msg}");
    assert!(msg.contains("1 component ID problem(s)"), "{msg}");
}

#[test]
fn component_ids_are_optional_by_default() {
    let toml = format!(
        "{CONTROLLERS}{}",
        r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a temperature sensor without a refdes"
"#
    );
    let report =
        analyze_with(&toml, settings(ControllerRole::Initiator)).unwrap();
    let descs: Vec<_> = report.device_descriptions().collect();
    assert_eq!(descs[0].device_id, None);

    let err = analyze_with(
        &toml,
        AnalysisSettings {
            require_component_ids: true,
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("has no component ID (refdes)"),
        "{err:#}"
    );
}

#[test]
fn error_component_id_too_long() {
    let toml = format!(
        "{CONTROLLERS}{}",
        r#"
[[i2c.devices]]
device = "tmp117"
refdes = ["J1", "U7"]
bus = "bus1"
address = 0x48
description = "a temperature sensor"
"#
    );
    // "J1/U7" is 5 bytes.
    analyze_with(
        &toml,
        AnalysisSettings {
            max_component_id_len: Some(5),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap();

    let err = analyze_with(
        &toml,
        AnalysisSettings {
            max_component_id_len: Some(4),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}")
            .contains("component ID \"J1/U7\" for device \"tmp117\" exceeds"),
        "{err:#}"
    );
}

#[test]
fn all_component_id_problems_are_reported_together() {
    let toml = format!(
        "{CONTROLLERS}{}",
        r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "no refdes"

[[i2c.devices]]
device = "tmp117"
refdes = "U1"
bus = "bus1"
address = 0x49
description = "first U1"

[[i2c.devices]]
device = "at24csw080"
refdes = "U1"
bus = "bus2"
address = 0x50
description = "second U1"

[[i2c.devices]]
device = "tmp117"
refdes = ["J100", "U200"]
bus = "bus2"
address = 0x4a
description = "too long"
"#
    );
    let err = analyze_with(
        &toml,
        AnalysisSettings {
            require_component_ids: true,
            max_component_id_len: Some(8),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("3 component ID problem(s)"), "{msg}");
    assert!(msg.contains("has no component ID"), "{msg}");
    assert!(msg.contains("duplicate component ID \"U1\""), "{msg}");
    assert!(
        msg.contains("\"J100/U200\" for device \"tmp117\" exceeds"),
        "{msg}"
    );
}

#[test]
fn vpd_kind_is_classified_by_device_type() {
    use build_i2c::{EepromVpd, VpdKind};
    let report = analyze(
        r#"
[[i2c.devices]]
device = "at24csw080"
refdes = "U1"
bus = "bus1"
address = 0x50
description = "default eeprom format"

[[i2c.devices]]
device = "at24csw080"
refdes = "U2"
bus = "bus1"
address = 0x51
description = "fan tray eeprom"
eeprom-vpd = "sled-fan-tray"

[[i2c.devices]]
device = "tmp117"
refdes = "U3"
bus = "bus1"
address = 0x48
description = "tmp11x"

[[i2c.devices]]
device = "tmp116"
refdes = "U4"
bus = "bus1"
address = 0x49
description = "tmp11x too"

[[i2c.devices]]
device = "max31790"
refdes = "U5"
bus = "bus2"
address = 0x20
description = "no vpd"
"#,
    )
    .unwrap();
    let vpd: Vec<_> = report.device_descriptions().map(|d| d.vpd).collect();
    assert_eq!(
        vpd,
        [
            Some(VpdKind::Eeprom(EepromVpd::SingleBarcode)),
            Some(VpdKind::Eeprom(EepromVpd::SledFanTray)),
            Some(VpdKind::Tmp11x),
            Some(VpdKind::Tmp11x),
            None,
        ]
    );
}

const OTHER_SENSORS: &str = r#"
[[sensor.devices]]
name = "dimm_a"
device = "ts0"
description = "DIMM A"
sensors = { temperature = 2 }
refdes = "J1"

[[sensor.devices]]
name = "fans"
device = "fpga"
description = "fan hub"
sensors = { speed = 3, temperature = 1 }
"#;

#[test]
fn other_sensors_follow_i2c_sensor_ids() {
    use build_i2c::Sensor;

    let report = analyze(&format!(
        r#"
[[i2c.devices]]
device = "tmp117"
name = "north"
refdes = "U7"
bus = "bus1"
address = 0x48
description = "an i2c temperature sensor"
sensors = {{ temperature = 1 }}
{OTHER_SENSORS}"#
    ))
    .unwrap();

    let s = &report.sensors;
    assert_eq!(s.total_i2c_sensors, 1);
    assert_eq!(s.total_other_sensors, 6);
    assert_eq!(s.by_id.len(), 7);

    // Non-I2C IDs continue after the I2C ones, in manifest order, and by
    // kind within a device (in `Sensor` order: temperature before speed).
    assert_eq!(s.other_sensors.len(), 2);
    let dimm = &s.other_sensors[0];
    assert_eq!(dimm.config.name, "dimm_a");
    assert_eq!(dimm.ids_by_kind[&Sensor::Temperature], vec![1, 2]);
    let fans = &s.other_sensors[1];
    assert_eq!(fans.ids_by_kind[&Sensor::Temperature], vec![3]);
    assert_eq!(fans.ids_by_kind[&Sensor::Speed], vec![4, 5, 6]);

    // They are all visible by ID, with their name and refdes.
    let sensor = s.by_id.get(&2).unwrap();
    assert_eq!(sensor.name.as_deref(), Some("dimm_a"));
    assert_eq!(sensor.kind, Sensor::Temperature);
    assert_eq!(
        sensor.refdes,
        Some(build_i2c::Refdes::Component("J1".into()))
    );
    assert_eq!(s.by_id.get(&6).unwrap().refdes, None);
}

#[test]
fn error_duplicate_other_sensor_name() {
    let err = analyze(
        r#"
[[sensor.devices]]
name = "dimm_a"
device = "ts0"
description = "DIMM A"
sensors = { temperature = 1 }

[[sensor.devices]]
name = "dimm_a"
device = "ts1"
description = "DIMM A again"
sensors = { temperature = 1 }
"#,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("Duplicate sensor name: dimm_a"),
        "{err:#}"
    );
}
